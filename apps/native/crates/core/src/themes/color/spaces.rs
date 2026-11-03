//! CSS Color 4 `color()` conversions, matching Culori's CSS spaces used by `themePalette.ts`.
use super::{Color, gamma, linear, split};
fn matrix(m: [[f64; 3]; 3], c: [f64; 3]) -> [f64; 3] {
    m.map(|row| row.into_iter().zip(c).map(|(a, b)| a * b).sum())
}
pub(super) fn parse(s: &str) -> Result<Color, String> {
    let error = || format!("Invalid literal CSS color: {s}");
    let body = s.strip_prefix("color(").and_then(|s| s.strip_suffix(')')).ok_or_else(error)?;
    let (space, args) = body.trim().split_once(char::is_whitespace).ok_or_else(error)?;
    let parts = split(args, '/');
    let number = |s: &str| -> Result<f64, String> {
        if s == "none" {
            return Ok(0.);
        }
        let n = s.trim_end_matches('%').parse::<f64>().map_err(|_| error())?;
        if !n.is_finite() {
            return Err(error());
        }
        Ok(if s.ends_with('%') { n / 100. } else { n })
    };
    let values = parts[0].split_whitespace().map(number).collect::<Result<Vec<_>, _>>()?;
    let [r, g, b] = values.as_slice() else { return Err(error()) };
    if parts.len() > 2 {
        return Err(error());
    }
    let alpha = if parts.len() == 2 { number(parts[1])?.clamp(0., 1.) } else { 1. };
    let c = [*r, *g, *b];
    let (mut xyz, d50) = match space {
        "srgb" => return Ok([*r, *g, *b, alpha]),
        "srgb-linear" => return Ok([gamma(*r), gamma(*g), gamma(*b), alpha]),
        "xyz" | "xyz-d65" => (c, false),
        "xyz-d50" => (c, true),
        "display-p3" => (
            matrix(
                [
                    [0.486570948648216, 0.265667693169093, 0.1982172852343625],
                    [0.2289745640697487, 0.6917385218365062, 0.079286914093745],
                    [0., 0.0451133818589026, 1.043944368900976],
                ],
                c.map(linear),
            ),
            false,
        ),
        "a98-rgb" => (
            matrix(
                [
                    [0.5766690429101305, 0.1855582379065463, 0.1882286462349947],
                    [0.297344975250536, 0.6273635662554661, 0.0752914584939979],
                    [0.0270313613864123, 0.0706888525358272, 0.9913375368376386],
                ],
                c.map(|v| v.signum() * v.abs().powf(563. / 256.)),
            ),
            false,
        ),
        "prophoto-rgb" => (
            matrix(
                [
                    [0.7977666449006423, 0.1351812974005331, 0.0313477341283922],
                    [0.2880748288194013, 0.7118352342418731, 0.0000899369387256],
                    [0., 0., 0.8251046025104602],
                ],
                c.map(|v| if v.abs() >= 16. / 512. { v.signum() * v.abs().powf(1.8) } else { v / 16. }),
            ),
            true,
        ),
        "rec2020" => (
            matrix(
                [
                    [0.6369580483012911, 0.1446169035862083, 0.1688809751641721],
                    [0.262700212011267, 0.6779980715188708, 0.059301716469862],
                    [0., 0.0280726930490874, 1.0609850577107909],
                ],
                c.map(|v| {
                    if v.abs() < 0.018053968510807 * 4.5 {
                        v / 4.5
                    } else {
                        v.signum() * ((v.abs() + 1.09929682680944 - 1.) / 1.09929682680944).powf(1. / 0.45)
                    }
                }),
            ),
            false,
        ),
        _ => return Err(error()),
    };
    if d50 {
        xyz = matrix(
            [
                [0.9554734527042182, -0.0230985368742614, 0.0632593086610217],
                [-0.0283697069632081, 1.0099954580058226, 0.021041398966943],
                [0.0123140016883199, -0.0205076964334779, 1.3303659366080753],
            ],
            xyz,
        );
    }
    let rgb = matrix(
        [
            [3.2409699419045226, -1.537_383_177_570_094, -0.4986107602930034],
            [-0.9692436362808796, 1.8759675015077204, 0.0415550574071756],
            [0.0556300796969936, -0.2039769588889765, 1.0569715142428784],
        ],
        xyz,
    )
    .map(gamma);
    Ok([rgb[0], rgb[1], rgb[2], alpha])
}
