use std::borrow::Cow;

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{AstcBlock, AstcChannel, Extent3d, TextureDimension, TextureFormat},
};
use image::Rgba32FImage;

const ASTC_BLOCK_SIZES: &[(AstcBlock, astcenc_rs::Extents)] = &[
    (AstcBlock::B4x4, astcenc_rs::Extents { x: 4, y: 4, z: 1 }),
    (AstcBlock::B5x4, astcenc_rs::Extents { x: 5, y: 4, z: 1 }),
    (AstcBlock::B5x5, astcenc_rs::Extents { x: 5, y: 5, z: 1 }),
    (AstcBlock::B6x5, astcenc_rs::Extents { x: 6, y: 5, z: 1 }),
    (AstcBlock::B6x6, astcenc_rs::Extents { x: 6, y: 6, z: 1 }),
    (AstcBlock::B8x5, astcenc_rs::Extents { x: 8, y: 5, z: 1 }),
    (AstcBlock::B8x6, astcenc_rs::Extents { x: 8, y: 6, z: 1 }),
    (AstcBlock::B8x8, astcenc_rs::Extents { x: 8, y: 8, z: 1 }),
    (AstcBlock::B10x5, astcenc_rs::Extents { x: 10, y: 5, z: 1 }),
    (AstcBlock::B10x6, astcenc_rs::Extents { x: 10, y: 6, z: 1 }),
    (AstcBlock::B10x8, astcenc_rs::Extents { x: 10, y: 8, z: 1 }),
    (
        AstcBlock::B10x10,
        astcenc_rs::Extents { x: 10, y: 10, z: 1 },
    ),
    (
        AstcBlock::B12x10,
        astcenc_rs::Extents { x: 12, y: 10, z: 1 },
    ),
    (
        AstcBlock::B12x12,
        astcenc_rs::Extents { x: 12, y: 12, z: 1 },
    ),
];

pub(crate) const fn extents(block_size: AstcBlock) -> Option<astcenc_rs::Extents> {
    let mut i = 0;

    while i < ASTC_BLOCK_SIZES.len() {
        let (check_block_size, extents) = ASTC_BLOCK_SIZES[i];
        if check_block_size as usize == block_size as usize {
            return Some(extents);
        }

        i += 1;
    }

    None
}

pub(crate) fn astc_convert(image: &Rgba32FImage, block_size: AstcBlock) -> Image {
    let extents = extents(block_size).unwrap();

    let config = astcenc_rs::ConfigBuilder::new()
        .with_profile(astcenc_rs::Profile::HdrRgbLdrA)
        .with_preset(astcenc_rs::PRESET_THOROUGH)
        .with_block_size(extents)
        .build()
        .unwrap();
    let mut context = astcenc_rs::Context::new(config).unwrap();

    let width = image.width().next_multiple_of(extents.x);
    let height = image.height().next_multiple_of(extents.y);

    let pixels = if width == image.width() && height == image.height() {
        Cow::Borrowed(&**image)
    } else {
        let pixels = image
            .rows()
            .enumerate()
            .flat_map(|(row_idx, row)| {
                let last = *image.get_pixel(image.width() - 1, row_idx as _);
                row.copied()
                    .chain(std::iter::repeat_n(last, (width - image.width()) as usize))
            })
            .chain({
                let last = *image.get_pixel(image.width() - 1, image.height() - 1);
                std::iter::repeat_n(
                    image
                        .rows()
                        .next_back()
                        .unwrap()
                        .copied()
                        .chain(std::iter::repeat_n(last, (width - image.width()) as usize)),
                    (height - image.height()) as usize,
                )
                .flatten()
            })
            .flat_map(|pixel| pixel.0)
            .collect::<Vec<_>>();

        Cow::Owned(pixels)
    };

    let image_to_encode = astcenc_rs::Image {
        extents: astcenc_rs::Extents {
            x: width,
            y: height,
            z: 1,
        },
        data: &[&*pixels][..],
    };

    let astc_bytes = context
        .compress(&image_to_encode, astcenc_rs::Swizzle::rgb1())
        .unwrap();

    #[cfg(feature = "humansize")]
    {
        info!(
            "Input lightmap size: {}",
            humansize::format_size(pixels.len(), humansize::DECIMAL),
        );
        info!(
            "ASTC lightmap size: {}",
            humansize::format_size(astc_bytes.len(), humansize::DECIMAL),
        );
    }

    #[cfg(not(feature = "humansize"))]
    {
        info!("Input lightmap size: {}b", pixels.len());
        info!("ASTC lightmap size: {}b", astc_bytes.len(),);
    }

    Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        astc_bytes,
        TextureFormat::Astc {
            block: block_size,
            channel: AstcChannel::Hdr,
        },
        RenderAssetUsages::RENDER_WORLD,
    )
}
