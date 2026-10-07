//! Persistent storage: settings and Wi-Fi credentials in a dedicated flash
//! partition (`storage`, see `partitions.csv`), managed by `sequential-storage`'s wear-levelled
//! key/value map.
//!
//! Keys: `1` settings JSON, `2` Wi-Fi credentials JSON, `3` flag bits. Writes are skipped when the
//! content did not change. Flash erase/write briefly stalls the CPU (and with it the radios), so
//! storage is only touched on explicit saves.

use alloc::vec;
use alloc::vec::Vec;
use embassy_embedded_hal::adapter::BlockingAsync;
use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{PARTITION_TABLE_MAX_LEN, read_partition_table};
use esp_storage::{FlashStorage, FlashStorageError};
use fitsim_core::settings::{Settings, WifiConfig};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};

const KEY_SETTINGS: u8 = 1;
const KEY_WIFI: u8 = 2;
const KEY_FLAGS: u8 = 3;

/// Boot into the provisioning access point once (set after repeated Wi-Fi failures).
pub const FLAG_PROVISION: u8 = 1;

const DATA_BUF: usize = 3584;

/// A window of the flash chip exposed as its own NOR flash device.
pub struct PartitionFlash {
    flash: FlashStorage<'static>,
    start: u32,
    len: u32,
}

impl ErrorType for PartitionFlash {
    type Error = FlashStorageError;
}

impl ReadNorFlash for PartitionFlash {
    const READ_SIZE: usize = <FlashStorage<'static> as ReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        ReadNorFlash::read(&mut self.flash, self.start + offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.len as usize
    }
}

impl NorFlash for PartitionFlash {
    const WRITE_SIZE: usize = <FlashStorage<'static> as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <FlashStorage<'static> as NorFlash>::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        NorFlash::erase(&mut self.flash, self.start + from, self.start + to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        NorFlash::write(&mut self.flash, self.start + offset, bytes)
    }
}

impl MultiwriteNorFlash for PartitionFlash {}

type NoCache = Cache<Uncached, Uncached, Uncached, u8>;
type Map = MapStorage<u8, BlockingAsync<PartitionFlash>, NoCache>;

pub struct Storage {
    map: Map,
    buf: Vec<u8>,
}

impl Storage {
    /// Locates the `storage` partition (or, failing that, any NVS data partition) and opens it.
    pub fn open(mut flash: FlashStorage<'static>) -> Option<Self> {
        let mut table_buf = vec![0u8; PARTITION_TABLE_MAX_LEN];
        let (start, len) = {
            let table = match read_partition_table(&mut flash, &mut table_buf) {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("storage: cannot read partition table: {e:?}");
                    return None;
                }
            };
            let by_label = table.iter().find(|p| p.label_as_str() == "storage");
            // Raw type 1 = data, subtype 2 = NVS. The default ESP-IDF table has one of these.
            let fallback = table
                .iter()
                .find(|p| p.raw_type() == 1 && p.raw_subtype() == 2);
            match by_label.or(fallback) {
                Some(p) => {
                    if by_label.is_none() {
                        log::warn!(
                            "storage: no 'storage' partition, using '{}' ({} bytes). Flash with crates/firmware/partitions.csv for full capacity",
                            p.label_as_str(),
                            p.len()
                        );
                    }
                    (p.offset(), p.len())
                }
                None => {
                    log::warn!("storage: no usable data partition, settings will not persist");
                    return None;
                }
            }
        };
        let page = <PartitionFlash as NorFlash>::ERASE_SIZE as u32;
        let len = len / page * page;
        let config = MapConfig::<BlockingAsync<PartitionFlash>>::try_new(0..len)
            .map_err(|e| log::warn!("storage: partition too small: {e:?}"))
            .ok()?;
        let flash = BlockingAsync::new(PartitionFlash { flash, start, len });
        log::info!("storage: {} KiB at 0x{start:x}", len / 1024);
        Some(Self {
            map: MapStorage::new(flash, config, NoCache::new_uncached()),
            buf: vec![0u8; DATA_BUF],
        })
    }

    async fn get(&mut self, key: u8) -> Option<Vec<u8>> {
        match self.map.fetch_item::<&[u8]>(&mut self.buf, &key).await {
            Ok(Some(v)) if !v.is_empty() => Some(v.to_vec()),
            Ok(_) => None,
            Err(e) => {
                log::warn!("storage: read of key {key} failed: {e:?}");
                None
            }
        }
    }

    async fn put(&mut self, key: u8, data: &[u8]) -> Result<(), &'static str> {
        self.map
            .store_item(&mut self.buf, &key, &data)
            .await
            .map_err(|e| {
                log::warn!("storage: write of key {key} failed: {e:?}");
                "flash write failed (storage full or corrupt)"
            })
    }

    pub async fn load_settings(&mut self) -> Settings {
        self.get(KEY_SETTINGS)
            .await
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .unwrap_or_default()
    }

    pub async fn save_settings(&mut self, s: &Settings) -> Result<(), &'static str> {
        let json = serde_json::to_vec(s).map_err(|_| "serialisation failed")?;
        if self.get(KEY_SETTINGS).await.as_deref() == Some(&json[..]) {
            return Ok(());
        }
        self.put(KEY_SETTINGS, &json).await
    }

    pub async fn load_wifi(&mut self) -> Option<WifiConfig> {
        let cfg: WifiConfig = serde_json::from_slice(&self.get(KEY_WIFI).await?).ok()?;
        cfg.validate().ok().map(|_| cfg)
    }

    pub async fn save_wifi(&mut self, cfg: &WifiConfig) -> Result<(), &'static str> {
        let json = serde_json::to_vec(cfg).map_err(|_| "serialisation failed")?;
        self.put(KEY_WIFI, &json).await
    }

    pub async fn flags(&mut self) -> u8 {
        self.get(KEY_FLAGS)
            .await
            .and_then(|b| b.first().copied())
            .unwrap_or(0)
    }

    pub async fn set_flags(&mut self, flags: u8) -> Result<(), &'static str> {
        self.put(KEY_FLAGS, &[flags]).await
    }

    /// Erases everything (factory reset).
    pub async fn erase_all(&mut self) -> Result<(), &'static str> {
        self.map.erase_all().await.map_err(|e| {
            log::warn!("storage: erase failed: {e:?}");
            "erase failed"
        })?;
        Ok(())
    }
}
