use bevy::prelude::*;

/// Metadata for a worldspawn face mesh (BSP geometry grouped by texture).
#[derive(Component, Debug, Clone)]
pub struct BspWorldspawnMesh {
    pub texture_name: String,
    /// The texture's Source `$surfaceprop` ("concrete", "wood", "metal", …),
    /// resolved from the VMT at spawn time. `None` when the material declares
    /// none — common for `decals/`, `tools/` and editor textures, and for the
    /// odd authored surface. See [`BspAsset::surface_prop`].
    ///
    /// [`BspAsset::surface_prop`]: crate::BspAsset::surface_prop
    pub surface_prop: Option<String>,
}

/// Metadata for a brush entity mesh (e.g. doors, func_detail).
#[derive(Component, Debug, Clone)]
pub struct BspBrushEntityMesh {
    pub texture_name: String,
    /// The texture's Source `$surfaceprop`; see [`BspWorldspawnMesh::surface_prop`].
    pub surface_prop: Option<String>,
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
