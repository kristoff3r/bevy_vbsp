pub mod info_player;

use bevy::prelude::*;

/// Metadata for a worldspawn face mesh (BSP geometry grouped by texture).
#[derive(Component, Debug, Clone)]
pub struct BspWorldspawnMesh {
    pub texture_name: String,
}

/// Metadata for a brush entity mesh (e.g. doors, func_detail).
#[derive(Component, Debug, Clone)]
pub struct BspBrushEntityMesh {
    pub texture_name: String,
    pub model_index: usize,
    pub classname: String,
}

/// Metadata for a MDL model mesh (prop entities defined in entity data).
#[derive(Component, Debug, Clone)]
pub struct BspEntityModelMesh {
    pub model_path: String,
    pub classname: String,
}

/// Metadata for a static prop mesh.
#[derive(Component, Debug, Clone)]
pub struct BspStaticPropMesh {
    pub model_path: String,
    pub prop_index: usize,
}
