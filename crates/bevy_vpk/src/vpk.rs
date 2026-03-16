use std::{
    io::SeekFrom,
    path::{Path, PathBuf},
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    task::{self, Poll},
};

use async_fs::File;
use bevy::{
    asset::{
        AsyncReadExt,
        io::{
            AssetReader, AssetReaderError, AssetSourceBuilder, PathStream, Reader,
            ReaderNotSeekableError, STACK_FUTURE_SIZE, SeekableReader, StackFuture,
        },
    },
    platform::collections::HashMap,
    prelude::*,
    tasks::futures_lite::{AsyncSeekExt, ready},
};
use futures_io::{AsyncRead, AsyncSeek};
use pin_project::pin_project;
use vpk::VPK;

pub struct VpkPlugin;

impl Plugin for VpkPlugin {
    fn build(&self, app: &mut App) {
        let reader = VpkAssetReader::default();

        app.insert_resource(reader.clone());

        let source =
            AssetSourceBuilder::new(move || Box::new(reader.clone())).with_writer(|_| None);

        app.register_asset_source("vpk", source);
        app.add_observer(load_vpk);
    }
}

fn load_vpk(
    added: On<LoadVpks>,
    vpk_reader: Res<VpkAssetReader>,
    mut commands: Commands,
) -> Result {
    info!("Loading VPKs: {:?}", added.paths);
    let paths = &added.paths;
    let vpk_reader = vpk_reader.clone();
    let mut vpk_reader = vpk_reader.vpks.lock().unwrap();
    for path in paths {
        let Ok(vpk) = vpk::from_path(path) else {
            warn!("Could not load VPK at {path:?}");
            continue;
        };
        vpk_reader.insert(path.clone(), vpk);
        info!("Loaded VPK: {:?}", path);
    }

    commands.trigger(LoadVPKDone);

    Ok(())
}

#[derive(Clone, Event)]
pub struct LoadVpks {
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Event)]
pub struct LoadVPKDone;

#[derive(Clone, Default, Resource)]
pub struct VpkAssetReader {
    vpks: Arc<Mutex<HashMap<PathBuf, VPK>>>,
}

impl AssetReader for VpkAssetReader {
    async fn read(&self, path: &Path) -> Result<impl Reader, AssetReaderError> {
        let path_str = path.to_str().expect("Path is not valid UTF-8");
        // VPK paths use `/` but Windows uses `\` by default, so we normalize here
        let path_str = path_str.replace(std::path::is_separator, "/");
        let path_str = &*path_str;

        let reader = 'block: {
            let vpks = self.vpks.lock().unwrap();

            for vpk in vpks.values() {
                let Some(entry) = vpk.tree.get(path_str) else {
                    continue;
                };

                let offset = entry.dir_entry.archive_offset;
                let file_length = entry.dir_entry.file_length;
                let preload_data = entry.preload_data.clone();
                let archive_path = entry.archive_path.clone();

                break 'block Some(VpkEntryReader {
                    file_length,
                    preload_data,
                    archive_path,
                    offset,
                    path: path.into(),
                    bytes_read: 0,
                    file: None,
                });
            }

            None
        };

        if let Some(mut reader) = reader {
            if let Some(archive_path) = &reader.archive_path {
                let mut f = File::open(archive_path.as_ref()).await?;
                f.seek(SeekFrom::Start(reader.offset as u64)).await?;
                reader.file = Some(f);
            }

            return Ok(reader);
        }

        Err(AssetReaderError::NotFound(path.into()))
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        Err::<Box<dyn Reader>, _>(AssetReaderError::NotFound(path.to_owned()))
    }
    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        Err(AssetReaderError::NotFound(path.to_owned()))
    }
    async fn is_directory<'a>(&'a self, _path: &'a Path) -> Result<bool, AssetReaderError> {
        Ok(false)
    }
    async fn read_meta_bytes<'a>(&'a self, path: &'a Path) -> Result<Vec<u8>, AssetReaderError> {
        Err(AssetReaderError::NotFound(path.to_owned()))
    }
}

#[pin_project]
pub struct VpkEntryReader {
    #[pin]
    preload_data: Vec<u8>,
    path: PathBuf,
    bytes_read: usize,
    file_length: u32,
    offset: u32,
    #[pin]
    file: Option<File>,
    archive_path: Option<Arc<PathBuf>>,
}

impl AsyncSeek for VpkEntryReader {
    fn poll_seek(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        pos: SeekFrom,
    ) -> Poll<futures_io::Result<u64>> {
        let offset = match pos {
            SeekFrom::Start(_) => todo!(),
            SeekFrom::End(_) => todo!(),
            SeekFrom::Current(offset) => offset as u64,
        };
        let Ok(result) = self
            .bytes_read
            .try_into()
            .map(|bytes_read: u64| bytes_read + offset)
        else {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek position is out of range",
            )));
        };
        let preload_data_len = self.preload_data.len() as u64;
        if result < preload_data_len {
            self.bytes_read = result as _;
            return Poll::Ready(Ok(self.bytes_read as _));
        }

        let this = self.project();

        if let Some(file) = this.file.as_pin_mut() {
            let result =
                ready!(file.poll_seek(cx, SeekFrom::Current((result - preload_data_len) as i64)))?;
            *this.bytes_read = (result + preload_data_len) as usize;
            return Poll::Ready(Ok(*this.bytes_read as _));
        }

        Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "seek position is out of range",
        )))
    }
}

impl AsyncRead for VpkEntryReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let preload_data_len = self.preload_data.len();
        if self.bytes_read < preload_data_len {
            let n = ready!(
                Pin::new(&mut &self.preload_data.as_slice()[self.bytes_read..]).poll_read(cx, buf)
            )?;
            self.bytes_read += n;
            return Poll::Ready(Ok(n));
        }

        let limit = self.file_length as u64 - (self.bytes_read - preload_data_len) as u64;
        // let this = self.project();
        if let Some(file) = self.file.as_mut() {
            let n = ready!(pin!(file.take(limit)).poll_read(cx, buf))?;
            self.bytes_read += n;
            return Poll::Ready(Ok(n));
        }

        Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "seek position is out of range",
        )))
    }
}

impl Reader for VpkEntryReader {
    fn read_to_end<'a>(
        &'a mut self,
        buf: &'a mut Vec<u8>,
    ) -> StackFuture<'a, std::io::Result<usize>, STACK_FUTURE_SIZE> {
        let future = bevy::tasks::futures_lite::AsyncReadExt::read_to_end(self, buf);
        StackFuture::from(future)
    }

    fn seekable(&mut self) -> Result<&mut dyn SeekableReader, ReaderNotSeekableError> {
        // TODO: make seekable
        Err(ReaderNotSeekableError)
    }
}
