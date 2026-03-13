use glam::Vec3;
use serde::Deserialize;
use vbsp::Angles;

#[derive(Deserialize)]
pub struct InfoPlayer {
    pub classname: String,
    pub origin: Vec3,
    pub angles: Angles,
}
