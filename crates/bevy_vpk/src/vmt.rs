use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    prelude::*,
};

#[derive(Default, TypePath)]
pub struct VmtAssetLoader;

#[derive(Asset, TypePath, Clone)]
pub struct VmtMaterial(pub vmt_parser::material::Material);

impl AssetLoader for VmtAssetLoader {
    type Asset = VmtMaterial;
    type Settings = ();
    type Error = anyhow::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let s = String::from_utf8_lossy(&bytes);
        Ok(VmtMaterial(vmt_parser::from_str(&s)?))
    }

    fn extensions(&self) -> &[&str] {
        &["vmt"]
    }
}
