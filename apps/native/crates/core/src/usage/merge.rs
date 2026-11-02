//! Totals across environments: `packages/shared/src/usageMerge.ts`, and the breakdown helpers in
//! `web/src/components/usage/usageBreakdown.ts`. A transcript directory two environments both
//! scan counts once, for the environment whose read is freshest.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::{UsageBucket, UsageCostSource, UsageProvider, UsageSource, UsageSourceFingerprint, UsageSourceStatus, UsageSummary, UsageTokenTotals};
use crate::sidebar::time::parse_iso;

/// One environment's answered summary.
#[derive(Debug, Clone)]
pub struct AnsweredUsage {
    pub environment_id: String,
    pub label: String,
    pub summary: Arc<UsageSummary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderTotals {
    pub provider: UsageProvider,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub records: u64,
    pub sessions: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelTotals {
    pub model: String,
    pub provider: UsageProvider,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub tokens: UsageTokenTotals,
    pub records: u64,
    pub unpriced_records: u64,
    pub unpriced_tokens: f64,
    pub cost_share: f64,
    pub token_share: f64,
}

impl ModelTotals {
    pub fn key(&self) -> String {
        format!("{}:{}", self.provider.as_str(), self.model)
    }

    /// `isModelCostUnknown`: every record lacks rates, so the cost is unknown, not zero.
    pub fn is_cost_unknown(&self) -> bool {
        self.records > 0 && self.unpriced_records >= self.records
    }

    /// `modelShare`: unknown cost has no cost share; tokens always do.
    pub fn share(&self, metric: ChartMetric) -> Option<f64> {
        match metric {
            ChartMetric::Tokens => Some(self.token_share),
            ChartMetric::Cost => (!self.is_cost_unknown()).then_some(self.cost_share),
        }
    }

    /// `cacheHitRate`: cache writes count as misses because their input was processed in full.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let input = self.tokens.uncached_input_tokens + self.tokens.cached_input_tokens + self.tokens.cache_creation_tokens;
        (input > 0).then(|| self.tokens.cached_input_tokens as f64 / input as f64)
    }

    /// `costPerMillionTokens`: effective USD per million priced tokens.
    pub fn cost_per_million_tokens(&self) -> Option<f64> {
        let priced = self.total_tokens as f64 - self.unpriced_tokens;
        (priced > 0. && !self.is_cost_unknown()).then(|| self.cost_usd / priced * 1_000_000.)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartMetric {
    Cost,
    Tokens,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PeriodProvider {
    pub cost_usd: f64,
    pub total_tokens: u64,
}

impl PeriodProvider {
    pub fn value(&self, metric: ChartMetric) -> f64 {
        match metric {
            ChartMetric::Cost => self.cost_usd,
            ChartMetric::Tokens => self.total_tokens as f64,
        }
    }
}

/// `DailyTotals` and `HourlyTotals`: `hour_start` is set only for hours.
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodTotals {
    pub day: String,
    pub hour_start: Option<String>,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: BTreeMap<UsageProvider, PeriodProvider>,
}

impl PeriodTotals {
    pub fn key(&self) -> &str {
        self.hour_start.as_deref().unwrap_or(&self.day)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CostQuality {
    pub provider_reported_share: f64,
    pub model_priced_share: f64,
    pub unpriced_share: f64,
    pub cache_savings_usd: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CategoryCost {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
    pub unsplit: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SpeedCost {
    pub standard: f64,
    pub fast: f64,
    pub ultrafast: f64,
    pub premium: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchDirection {
    ServerBehind,
    ClientBehind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContractMismatch {
    pub environment_id: String,
    pub direction: MismatchDirection,
    pub contract_version: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergedUsage {
    pub cost_usd: f64,
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub records: u64,
    pub sessions: u64,
    pub providers: Vec<ProviderTotals>,
    pub models: Vec<ModelTotals>,
    pub daily: Vec<PeriodTotals>,
    pub hourly: Vec<PeriodTotals>,
    pub cost_quality: CostQuality,
    pub category_cost: CategoryCost,
    pub speed_cost: SpeedCost,
    pub duplicate_sources: Vec<String>,
    pub contributing_environments: Vec<String>,
    pub contract_mismatches: Vec<ContractMismatch>,
}

impl MergedUsage {
    pub fn provider(&self, provider: UsageProvider) -> Option<&ProviderTotals> {
        self.providers.iter().find(|entry| entry.provider == provider)
    }

    pub fn token_totals(&self) -> UsageTokenTotals {
        UsageTokenTotals {
            uncached_input_tokens: self.uncached_input_tokens,
            cached_input_tokens: self.cached_input_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens,
        }
    }
}

pub fn is_compatible_contract_version(version: f64, expected: f64) -> bool {
    version >= super::USAGE_MERGE_COMPATIBLE_SINCE && version <= expected
}

type Fingerprint = (String, UsageProvider, String, String);

fn fingerprint_key(fingerprint: &UsageSourceFingerprint) -> Fingerprint {
    (fingerprint.host_id.clone(), fingerprint.provider, fingerprint.resolved_home_path.clone(), fingerprint.volume_id.clone())
}

fn buckets_for_source<'a>(summary: &'a UsageSummary, source: &UsageSource) -> impl Iterator<Item = (usize, &'a UsageBucket)> {
    let provider = source.fingerprint.provider;
    let single = summary.sources.iter().filter(|entry| entry.fingerprint.provider == provider).count() == 1;
    let home = source.fingerprint.resolved_home_path.clone();
    summary.buckets.iter().enumerate().filter(move |(_, bucket)| {
        bucket.provider == provider && (bucket.source_path.as_deref() == Some(home.as_str()) || (bucket.source_path.is_none() && single))
    })
}

type Cell = (String, Option<String>, UsageProvider, String);

fn bucket_cell(bucket: &UsageBucket) -> Cell {
    (bucket.day.clone(), bucket.hour_start.clone(), bucket.provider, bucket.model.clone())
}

struct Claims {
    owner: HashMap<Fingerprint, usize>,
    supplemental: HashMap<usize, HashSet<usize>>,
    sessions: HashMap<Fingerprint, u64>,
    duplicates: Vec<String>,
}

fn read_at(environment: &AnsweredUsage) -> Option<i64> {
    parse_iso(&environment.summary.read_at)
}

fn claim_sources(environments: &[&AnsweredUsage]) -> Claims {
    let mut owner: HashMap<Fingerprint, usize> = HashMap::new();
    let mut owner_scan: HashMap<Fingerprint, (usize, usize)> = HashMap::new();
    let mut seen_cells: HashMap<Fingerprint, HashSet<Cell>> = HashMap::new();
    let mut supplemental: HashMap<usize, HashSet<usize>> = HashMap::new();
    let mut sessions: HashMap<Fingerprint, u64> = HashMap::new();
    let mut duplicates = Vec::new();

    let mut ordered: Vec<usize> = (0..environments.len()).collect();
    ordered.sort_by(|&a, &b| {
        let (left, right) = (read_at(environments[a]).unwrap_or(0), read_at(environments[b]).unwrap_or(0));
        right.cmp(&left).then_with(|| environments[a].environment_id.cmp(&environments[b].environment_id))
    });

    for status in [UsageSourceStatus::Ok, UsageSourceStatus::Partial, UsageSourceStatus::Failed] {
        for &index in &ordered {
            let environment = environments[index];
            for (source_index, source) in environment.summary.sources.iter().enumerate() {
                if source.status != status {
                    continue;
                }
                let key = fingerprint_key(&source.fingerprint);
                if owner.contains_key(&key) {
                    duplicates.push(format!("{}: {}", environment.label, source.fingerprint.resolved_home_path));
                    continue;
                }
                owner.insert(key.clone(), index);
                owner_scan.insert(key.clone(), (index, source_index));
                sessions.insert(key, source.distinct_sessions);
            }
        }
    }

    // Keep cells unique to newer partial scans; aggregates cannot reconcile overlapping records.
    for &index in &ordered {
        let environment = environments[index];
        for source in environment.summary.sources.iter().filter(|source| source.status == UsageSourceStatus::Partial) {
            let key = fingerprint_key(&source.fingerprint);
            let Some(&(owner_index, owner_source)) = owner_scan.get(&key) else { continue };
            let owner_environment = environments[owner_index];
            let owner_source = &owner_environment.summary.sources[owner_source];
            let newer = match (read_at(environment), read_at(owner_environment)) {
                (Some(mine), Some(theirs)) => mine > theirs,
                _ => true,
            };
            // A scan under another model filter counts different records, so its cells are not missing ones.
            if owner_source.status != UsageSourceStatus::Ok || !newer || environment.summary.model_filter != owner_environment.summary.model_filter {
                continue;
            }
            let seen = seen_cells
                .entry(key.clone())
                .or_insert_with(|| buckets_for_source(&owner_environment.summary, owner_source).map(|(_, bucket)| bucket_cell(bucket)).collect());
            let mut added = false;
            for (bucket_index, bucket) in buckets_for_source(&environment.summary, source) {
                if seen.insert(bucket_cell(bucket)) {
                    supplemental.entry(index).or_default().insert(bucket_index);
                    added = true;
                }
            }
            if added {
                let current = sessions.get(&key).copied().unwrap_or(0);
                sessions.insert(key, current.max(source.distinct_sessions));
            }
        }
    }

    Claims { owner, supplemental, sessions, duplicates }
}

struct ProviderAccumulator {
    cost_usd: f64,
    total_tokens: u64,
    records: u64,
    sessions: u64,
}

struct ModelAccumulator {
    provider: UsageProvider,
    model: String,
    cost_usd: f64,
    total_tokens: u64,
    tokens: UsageTokenTotals,
    records: u64,
    unpriced_records: u64,
    unpriced_tokens: f64,
}

/// Insertion-ordered accumulators, so ties sort as the web's `Map` iteration does.
struct Ordered<K, V> {
    index: HashMap<K, usize>,
    entries: Vec<(K, V)>,
}

impl<K: std::hash::Hash + Eq + Clone, V> Ordered<K, V> {
    fn new() -> Self {
        Ordered { index: HashMap::new(), entries: Vec::new() }
    }

    fn entry(&mut self, key: &K, make: impl FnOnce() -> V) -> &mut V {
        let position = match self.index.get(key) {
            Some(&position) => position,
            None => {
                self.entries.push((key.clone(), make()));
                self.index.insert(key.clone(), self.entries.len() - 1);
                self.entries.len() - 1
            }
        };
        &mut self.entries[position].1
    }
}

fn add_period(period: &mut PeriodTotals, bucket: &UsageBucket, tokens: u64) {
    period.cost_usd += bucket.cost_usd;
    period.total_tokens += tokens;
    let provider = period.by_provider.entry(bucket.provider).or_default();
    provider.cost_usd += bucket.cost_usd;
    provider.total_tokens += tokens;
}

/// `mergeUsage`.
pub fn merge_usage(environments: &[AnsweredUsage], expected_contract_version: f64) -> MergedUsage {
    if environments.is_empty() {
        return MergedUsage::default();
    }
    let mut current: Vec<&AnsweredUsage> = Vec::new();
    let mut contract_mismatches = Vec::new();
    for environment in environments {
        let version = environment.summary.contract_version;
        if is_compatible_contract_version(version, expected_contract_version) {
            current.push(environment);
        } else {
            contract_mismatches.push(ContractMismatch {
                environment_id: environment.environment_id.clone(),
                direction: if version < expected_contract_version { MismatchDirection::ServerBehind } else { MismatchDirection::ClientBehind },
                contract_version: version,
            });
        }
    }

    let claims = claim_sources(&current);
    let mut merged = MergedUsage { contract_mismatches, duplicate_sources: claims.duplicates.clone(), ..MergedUsage::default() };
    let (mut cache_savings_usd, mut provider_reported_records, mut unpriced_records) = (0., 0u64, 0u64);
    let mut category = CategoryCost::default();
    let mut speed = SpeedCost::default();
    let mut providers: Ordered<UsageProvider, ProviderAccumulator> = Ordered::new();
    let mut models: Ordered<(UsageProvider, String), ModelAccumulator> = Ordered::new();
    let mut daily: HashMap<String, PeriodTotals> = HashMap::new();
    let mut hourly: HashMap<String, PeriodTotals> = HashMap::new();
    let empty = HashSet::new();
    let new_provider = || ProviderAccumulator { cost_usd: 0., total_tokens: 0, records: 0, sessions: 0 };

    for (index, environment) in current.iter().enumerate() {
        let supplemental = claims.supplemental.get(&index).unwrap_or(&empty);
        let mut owned_providers = HashSet::new();
        let mut owned_sources = HashSet::new();
        let mut sessions_by_provider: Ordered<UsageProvider, u64> = Ordered::new();
        for source in environment.summary.sources.iter().filter(|source| source.status != UsageSourceStatus::Missing) {
            let key = fingerprint_key(&source.fingerprint);
            if claims.owner.get(&key) != Some(&index) {
                continue;
            }
            let provider = source.fingerprint.provider;
            owned_providers.insert(provider);
            owned_sources.insert((provider, source.fingerprint.resolved_home_path.clone()));
            // Distinct sessions per directory; bucket sums overcount sessions spanning days or models.
            *sessions_by_provider.entry(&provider, || 0) += claims.sessions.get(&key).copied().unwrap_or(source.distinct_sessions);
        }
        let buckets: Vec<&UsageBucket> = environment
            .summary
            .buckets
            .iter()
            .enumerate()
            .filter(|(bucket_index, bucket)| {
                supplemental.contains(bucket_index)
                    || match &bucket.source_path {
                        None => owned_providers.contains(&bucket.provider),
                        Some(path) => owned_sources.contains(&(bucket.provider, path.clone())),
                    }
            })
            .map(|(_, bucket)| bucket)
            .collect();
        if !buckets.is_empty() {
            merged.contributing_environments.push(environment.environment_id.clone());
        }
        for (provider, sessions) in sessions_by_provider.entries {
            merged.sessions += sessions;
            if sessions > 0 {
                providers.entry(&provider, new_provider).sessions += sessions;
            }
        }
        for bucket in buckets {
            let tokens = bucket.tokens();
            merged.cost_usd += bucket.cost_usd;
            cache_savings_usd += bucket.cache_savings_usd;
            merged.uncached_input_tokens += bucket.totals.uncached_input_tokens;
            merged.cached_input_tokens += bucket.totals.cached_input_tokens;
            merged.cache_creation_tokens += bucket.totals.cache_creation_tokens;
            merged.output_tokens += bucket.totals.output_tokens;
            merged.reasoning_tokens += bucket.totals.reasoning_tokens;
            merged.records += bucket.records;
            unpriced_records += bucket.unpriced_records;
            if bucket.cost_source == UsageCostSource::ProviderReported {
                provider_reported_records += bucket.records;
            }
            if let Some(cost) = &bucket.category_cost_usd {
                category.input += cost.input;
                category.cache_read += cost.cache_read;
                category.cache_write += cost.cache_write;
                category.output += cost.output;
            }
            speed.fast += bucket.fast_cost_usd.unwrap_or(0.);
            speed.ultrafast += bucket.ultrafast_cost_usd.unwrap_or(0.);
            speed.premium += bucket.speed_premium_usd.unwrap_or(0.);

            let provider = providers.entry(&bucket.provider, new_provider);
            provider.cost_usd += bucket.cost_usd;
            provider.total_tokens += tokens;
            provider.records += bucket.records;

            let model = models.entry(&(bucket.provider, bucket.model.clone()), || ModelAccumulator {
                provider: bucket.provider,
                model: bucket.model.clone(),
                cost_usd: 0.,
                total_tokens: 0,
                tokens: UsageTokenTotals::default(),
                records: 0,
                unpriced_records: 0,
                unpriced_tokens: 0.,
            });
            model.cost_usd += bucket.cost_usd;
            model.total_tokens += tokens;
            model.tokens.uncached_input_tokens += bucket.totals.uncached_input_tokens;
            model.tokens.cached_input_tokens += bucket.totals.cached_input_tokens;
            model.tokens.cache_creation_tokens += bucket.totals.cache_creation_tokens;
            model.tokens.output_tokens += bucket.totals.output_tokens;
            model.tokens.reasoning_tokens += bucket.totals.reasoning_tokens;
            model.records += bucket.records;
            model.unpriced_records += bucket.unpriced_records;
            if bucket.records > 0 {
                model.unpriced_tokens += tokens as f64 * bucket.unpriced_records as f64 / bucket.records as f64;
            }

            let day = daily.entry(bucket.day.clone()).or_insert_with(|| PeriodTotals {
                day: bucket.day.clone(),
                hour_start: None,
                cost_usd: 0.,
                total_tokens: 0,
                by_provider: BTreeMap::new(),
            });
            add_period(day, bucket, tokens);
            if let Some(hour_start) = &bucket.hour_start {
                let hour = hourly.entry(hour_start.clone()).or_insert_with(|| PeriodTotals {
                    day: bucket.day.clone(),
                    hour_start: Some(hour_start.clone()),
                    cost_usd: 0.,
                    total_tokens: 0,
                    by_provider: BTreeMap::new(),
                });
                add_period(hour, bucket, tokens);
            }
        }
    }

    let total_tokens = merged.uncached_input_tokens + merged.cached_input_tokens + merged.cache_creation_tokens + merged.output_tokens;
    merged.total_tokens = total_tokens;
    let share = |part: f64, whole: f64| if whole == 0. { 0. } else { part / whole };
    let cost_usd = merged.cost_usd;
    merged.providers = providers
        .entries
        .into_iter()
        .map(|(provider, totals)| ProviderTotals {
            provider,
            cost_usd: totals.cost_usd,
            total_tokens: totals.total_tokens,
            records: totals.records,
            sessions: totals.sessions,
            cost_share: share(totals.cost_usd, cost_usd),
            token_share: share(totals.total_tokens as f64, total_tokens as f64),
        })
        .collect();
    merged.providers.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd));
    merged.models = models
        .entries
        .into_iter()
        .map(|(_, totals)| ModelTotals {
            cost_share: share(totals.cost_usd, cost_usd),
            token_share: share(totals.total_tokens as f64, total_tokens as f64),
            model: totals.model,
            provider: totals.provider,
            cost_usd: totals.cost_usd,
            total_tokens: totals.total_tokens,
            tokens: totals.tokens,
            records: totals.records,
            unpriced_records: totals.unpriced_records,
            unpriced_tokens: totals.unpriced_tokens,
        })
        .collect();
    merged.models.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd).then(b.total_tokens.cmp(&a.total_tokens)));
    merged.daily = daily.into_values().collect();
    merged.daily.sort_by(|a, b| a.day.cmp(&b.day));
    merged.hourly = hourly.into_values().collect();
    merged.hourly.sort_by(|a, b| a.hour_start.cmp(&b.hour_start));

    let records = merged.records as f64;
    merged.cost_quality = CostQuality {
        provider_reported_share: share(provider_reported_records as f64, records),
        unpriced_share: share(unpriced_records as f64, records),
        model_priced_share: if records == 0. { 0. } else { (records - provider_reported_records as f64 - unpriced_records as f64) / records },
        cache_savings_usd,
    };
    category.unsplit = (cost_usd - category.input - category.cache_read - category.cache_write - category.output).max(0.);
    merged.category_cost = category;
    speed.standard = (cost_usd - speed.fast - speed.ultrafast).max(0.);
    merged.speed_cost = speed;
    merged
}

/// `sortModelsByTokens`.
pub fn sort_models_by_tokens(models: &[ModelTotals]) -> Vec<ModelTotals> {
    let mut sorted = models.to_vec();
    sorted.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens).then(b.cost_usd.total_cmp(&a.cost_usd)));
    sorted
}

/// The share-bar fills: `color-mix(in oklab, foreground N%, background)` and its legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    P30,
    P34,
    P44,
    P60,
    P66,
    P72,
    P100,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShareSegment {
    pub label: &'static str,
    pub value: f64,
    pub ink: Ink,
}

/// `costTypeSegments`: unsplit cost below half a cent reads as rounding, not a category.
pub fn cost_type_segments(cost: &CategoryCost) -> Vec<ShareSegment> {
    vec![
        ShareSegment { label: "Input", value: cost.input, ink: Ink::P60 },
        ShareSegment { label: "Cache read", value: cost.cache_read, ink: Ink::P30 },
        ShareSegment { label: "Cache write", value: cost.cache_write, ink: Ink::P72 },
        ShareSegment { label: "Output", value: cost.output, ink: Ink::P100 },
        ShareSegment { label: "Other", value: if cost.unsplit >= 0.005 { cost.unsplit } else { 0. }, ink: Ink::P44 },
    ]
}

/// `tokenTypeSegments`.
pub fn token_type_segments(tokens: &UsageTokenTotals) -> Vec<ShareSegment> {
    vec![
        ShareSegment { label: "Input", value: tokens.uncached_input_tokens as f64, ink: Ink::P60 },
        ShareSegment { label: "Cache read", value: tokens.cached_input_tokens as f64, ink: Ink::P30 },
        ShareSegment { label: "Cache write", value: tokens.cache_creation_tokens as f64, ink: Ink::P72 },
        ShareSegment { label: "Output", value: tokens.output_tokens as f64, ink: Ink::P100 },
    ]
}

/// `speedCostSegments`.
pub fn speed_cost_segments(cost: &SpeedCost) -> Vec<ShareSegment> {
    vec![
        ShareSegment { label: "Standard", value: cost.standard, ink: Ink::P34 },
        ShareSegment { label: "Fast", value: cost.fast, ink: Ink::P66 },
        ShareSegment { label: "Ultrafast", value: cost.ultrafast, ink: Ink::P100 },
    ]
}

/// The segments a share bar draws: those with a value, and their total.
pub fn visible_segments(segments: &[ShareSegment]) -> (Vec<ShareSegment>, f64) {
    let visible: Vec<ShareSegment> = segments.iter().filter(|segment| segment.value > 0.).cloned().collect();
    let total = visible.iter().map(|segment| segment.value).sum();
    (visible, total)
}
