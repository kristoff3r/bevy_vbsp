use bevy::{
    asset::{AssetLoader, LoadContext, RenderAssetUsages, io::Reader},
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
};

#[derive(Default, TypePath)]
pub struct VtfAssetLoader;

impl AssetLoader for VtfAssetLoader {
    type Asset = Image;
    type Settings = ();
    type Error = vtf::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;

        let vtf = vtf::from_bytes(&bytes)?;
        let image = vtf.highres_image.decode(0)?;

        let mut texture = Image::from_dynamic(image, true, RenderAssetUsages::RENDER_WORLD);
        texture.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            anisotropy_clamp: 16,
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::linear()
        });

        Ok(texture)
    }
    fn extensions(&self) -> &[&str] {
        &["vtf"]
    }
}
