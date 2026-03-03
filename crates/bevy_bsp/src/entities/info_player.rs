use serde::Deserialize;
use vbsp::{Angles, Vector};

#[derive(Deserialize)]
pub struct InfoPlayer {
    pub classname: String,
    pub origin: Vector,
    pub angles: Angles,
}
