use std::str::FromStr;

use bevy::math::Vec3;

/// Parse a space-separated `"x y z"` string into a [`Vec3`].
pub fn parse_vec3(s: &str) -> Result<Vec3, <f32 as FromStr>::Err> {
    let mut parts = s.split(' ');
    Ok([
        parts.next().unwrap_or_default().parse()?,
        parts.next().unwrap_or_default().parse()?,
        parts.next().unwrap_or_default().parse()?,
    ]
    .into())
}
