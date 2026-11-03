//! CSS Color 4 interpolation used by `index.css` and `themePalette.ts`.

pub type Color = [f64; 4];
pub fn parse(s: &str) -> Result<Color, String> {
    let s = s.trim();
    for name in ["oklch", "oklab"] {
        if let Some(args) = s.strip_prefix(&format!("{name}(")).and_then(|s| s.strip_suffix(')')) {
            let parts = split(args, '/');
            let words = parts[0].split_whitespace().collect::<Vec<_>>();
            if words.len() != 3 || parts.len() > 2 {
                return Err(format!("Invalid literal CSS color: {s}"));
            }
            let number = |word: &str, scale: f64| -> Result<f64, String> {
                if word == "none" {
                    return Ok(0.);
                }
                let n = word.trim_end_matches('%').parse::<f64>().map_err(|e| e.to_string())?;
                if !n.is_finite() {
                    return Err("Non-finite color".into());
                }
                Ok(if word.ends_with('%') { n / 100. * scale } else { n })
            };
            let l = number(words[0], 1.)?.clamp(0., 1.);
            let a = number(words[1], 0.4)?;
            let alpha = if parts.len() == 2 { number(parts[1], 1.)?.clamp(0., 1.) } else { 1. };
            if name == "oklch" {
                let h = words[2];
                let hue = if let Some(h) = h.strip_suffix("rad") {
                    number(h, 1.)?.to_degrees()
                } else if let Some(h) = h.strip_suffix("turn") {
                    number(h, 1.)? * 360.
                } else {
                    number(h.trim_end_matches("deg"), 1.)?
                };
                let mut c = from_lch([l, a.max(0.), hue]);
                c[3] = alpha;
                return Ok(c);
            }
            return Ok(from_lab([l, a, number(words[2], 0.4)?], alpha));
        }
    }
    let c = s.parse::<csscolorparser::Color>().map_err(|_| format!("Invalid literal CSS color: {s}"))?;
    Ok([c.r.into(), c.g.into(), c.b.into(), c.a.into()])
}
fn linear(x: f64) -> f64 {
    x.signum() * if x.abs() <= 0.04045 { x.abs() / 12.92 } else { ((x.abs() + 0.055) / 1.055).powf(2.4) }
}
fn gamma(x: f64) -> f64 {
    x.signum() * if x.abs() <= 0.0031308 { x.abs() * 12.92 } else { 1.055 * x.abs().powf(1. / 2.4) - 0.055 }
}
pub fn lab(c: Color) -> [f64; 3] {
    let [r, g, b, _] = c.map(linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
    ]
}
pub fn from_lab([l, a, b]: [f64; 3], alpha: f64) -> Color {
    let x = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let y = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let z = (l - 0.0894841775 * a - 1.291485548 * b).powi(3);
    [
        gamma(4.0767416621 * x - 3.3077115913 * y + 0.2309699292 * z),
        gamma(-1.2684380046 * x + 2.6097574011 * y - 0.3413193965 * z),
        gamma(-0.0041960863 * x - 0.7034186147 * y + 1.707614701 * z),
        alpha,
    ]
}
pub fn lch(c: Color) -> [f64; 3] {
    let [l, a, b] = lab(c);
    [l, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.)]
}
pub fn from_lch([l, c, h]: [f64; 3]) -> Color {
    from_lab([l, c * h.to_radians().cos(), c * h.to_radians().sin()], 1.)
}
fn cie(c: Color) -> [f64; 3] {
    let [r, g, b, _] = c.map(linear);
    let f = |v: f64| {
        if v > 216. / 24389. { v.cbrt() } else { ((24389. / 27.) * v + 16.) / 116. }
    };
    let x = f((0.4360747 * r + 0.3850649 * g + 0.1430804 * b) / (0.3457 / 0.3585));
    let y = f(0.2225045 * r + 0.7168786 * g + 0.0606169 * b);
    let z = f((0.0139322 * r + 0.0971045 * g + 0.7141733 * b) / ((1. - 0.3457 - 0.3585) / 0.3585));
    [116. * y - 16., 500. * (x - y), 200. * (y - z)]
}
fn from_cie([l, a, b]: [f64; 3], alpha: f64) -> Color {
    let fy = (l + 16.) / 116.;
    let fx = a / 500. + fy;
    let fz = fy - b / 200.;
    let inv = |v: f64| {
        if v.powi(3) > 216. / 24389. { v.powi(3) } else { (116. * v - 16.) / (24389. / 27.) }
    };
    let x = inv(fx) * (0.3457 / 0.3585);
    let y = inv(fy);
    let z = inv(fz) * ((1. - 0.3457 - 0.3585) / 0.3585);
    [
        gamma(3.1338561 * x - 1.6168667 * y - 0.4906146 * z),
        gamma(-0.9787684 * x + 1.9161415 * y + 0.033454 * z),
        gamma(0.0719453 * x - 0.2289914 * y + 1.4052427 * z),
        alpha,
    ]
}
pub fn mix(a: Color, b: Color, weight: f64, space: &str) -> Color {
    if weight == 1. {
        return a;
    }
    if weight == 0. {
        return b;
    }
    let alpha = a[3] * weight + b[3] * (1. - weight);
    if alpha == 0. {
        return [0.; 4];
    }
    let convert = |c: Color| match space {
        "oklab" => lab(c),
        "oklch" => lch(c),
        "lab" => cie(c),
        _ => [c[0], c[1], c[2]],
    };
    let x = convert(a);
    let y = convert(b);
    let mut out = std::array::from_fn(|i| (x[i] * a[3] * weight + y[i] * b[3] * (1. - weight)) / alpha);
    if space == "oklch" {
        let mut h1 = x[2];
        let mut h2 = y[2];
        if x[1] < 1e-6 {
            h1 = h2;
        }
        if y[1] < 1e-6 {
            h2 = h1;
        }
        if h2 - h1 > 180. {
            h1 += 360.;
        } else if h1 - h2 > 180. {
            h2 += 360.;
        }
        out[2] = (h1 * weight + h2 * (1. - weight)).rem_euclid(360.);
    }
    match space {
        "oklab" => from_lab(out, alpha),
        "oklch" => {
            let mut c = from_lch(out);
            c[3] = alpha;
            c
        }
        "lab" => from_cie(out, alpha),
        _ => [out[0], out[1], out[2], alpha],
    }
}
pub fn gamut(mut c: [f64; 3]) -> [f64; 3] {
    let fits = |c| {
        from_lch(c)[..3].iter().all(|v| {
            let v = linear(*v);
            (-0.0001..=1.0001).contains(&v)
        })
    };
    if fits(c) {
        return c;
    }
    let mut low = 0.;
    let mut high = c[1];
    let steps = (high.max(0.000001).log2() - 0.000001f64.log2()).ceil().max(1.) as usize;
    for _ in 0..steps {
        c[1] = (low + high) / 2.;
        if fits(c) {
            low = c[1];
        } else {
            high = c[1];
        }
    }
    c[1] = low;
    c
}
pub fn mapped(c: Color, round: bool) -> Color {
    let mut out = from_lch(gamut(lch(c)));
    out[3] = c[3];
    for v in &mut out[..3] {
        *v = v.clamp(0., 1.);
        if round {
            *v = (*v * 255.).round() / 255.;
        }
    }
    out
}
pub fn packed(c: Color) -> u32 {
    c.into_iter().fold(0, |n, v| (n << 8) | (v.clamp(0., 1.) * 255.).round() as u32)
}
pub fn css(c: Color) -> String {
    format!("#{:08x}", packed(c))
}
pub fn luminance(c: Color) -> f64 {
    0.2126 * linear(c[0]) + 0.7152 * linear(c[1]) + 0.0722 * linear(c[2])
}
pub fn contrast(a: Color, b: Color) -> f64 {
    let x = luminance(a);
    let y = luminance(b);
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}
pub fn solve(mut c: [f64; 3], surfaces: &[Color], ratio: f64, lighter: bool) -> Color {
    let fits = |c| surfaces.iter().all(|s| contrast(mapped(from_lch(c), true), *s) >= ratio);
    if fits(c) {
        return from_lch(gamut(c));
    }
    let (mut low, mut high) = if lighter { (c[0], 1.) } else { (0., c[0]) };
    for _ in 0..18 {
        c[0] = (low + high) / 2.;
        if fits(c) == lighter {
            high = c[0];
        } else {
            low = c[0];
        }
    }
    c[0] = if lighter { high } else { low };
    from_lch(gamut(c))
}
pub fn split(s: &str, sep: char) -> Vec<&str> {
    let mut depth = 0;
    let mut start = 0;
    let mut out = Vec::new();
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && c == sep {
            out.push(s[start..i].trim());
            start = i + c.len_utf8();
        }
    }
    out.push(s[start..].trim());
    out
}
pub fn evaluate(s: &str, roles: &std::collections::BTreeMap<String, String>, dark: bool) -> Result<Color, String> {
    let s = s.trim();
    if let Some(key) = s.strip_prefix("role(").and_then(|s| s.strip_suffix(')')) {
        return parse(roles.get(key).ok_or_else(|| format!("Missing role {key}"))?);
    }
    if let Some(args) = s.strip_prefix("light-dark(").and_then(|s| s.strip_suffix(')')) {
        let p = split(args, ',');
        return evaluate(p[usize::from(dark)], roles, dark);
    }
    if let Some(args) = s.strip_prefix("--alpha(").and_then(|s| s.strip_suffix(')')) {
        let p = split(args, '/');
        let mut c = evaluate(p[0], roles, dark)?;
        c[3] *= pct(p[1])?;
        return Ok(c);
    }
    if let Some(args) = s.strip_prefix("color-mix(").and_then(|s| s.strip_suffix(')')) {
        let p = split(args, ',');
        let operand = |s: &str| -> Result<(Color, Option<f64>), String> {
            let w = split(s, ' ');
            let last = w.last().copied().unwrap_or("");
            if last.ends_with('%') {
                let at = s.rfind(last).unwrap();
                Ok((evaluate(s[..at].trim(), roles, dark)?, Some(pct(last)?)))
            } else {
                Ok((evaluate(s, roles, dark)?, None))
            }
        };
        let (a, wa) = operand(p[1])?;
        let (b, wb) = operand(p[2])?;
        let wa = wa.unwrap_or_else(|| 1. - wb.unwrap_or(0.5));
        let wb = wb.unwrap_or(1. - wa);
        let sum = wa + wb;
        if sum <= 0. {
            return Err("Invalid color-mix weights".into());
        }
        let mut c = mix(a, b, wa / sum, p[0].trim_start_matches("in "));
        c[3] *= sum.min(1.);
        return Ok(c);
    }
    parse(s)
}
fn pct(s: &str) -> Result<f64, String> {
    s.trim_end_matches('%').parse::<f64>().map(|n| n / 100.).map_err(|e| e.to_string())
}
