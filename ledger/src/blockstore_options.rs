use rocksdb::{DBCompressionType as RocksCompressionType, DBRecoveryMode};

pub struct BlockstoreOptions {
    // The access type of blockstore. Default: Primary
    pub access_type: AccessType,
    // Whether to open a blockstore under a recovery mode. Default: None.
    pub recovery_mode: Option<BlockstoreRecoveryMode>,
    // Whether to allow unlimited number of open files. Default: true.
    pub enforce_ulimit_nofile: bool,
    pub column_options: LedgerColumnOptions,
}

impl Default for BlockstoreOptions {
    /// The default options are the values used by [`Blockstore::open`].
    fn default() -> Self {
        Self {
            access_type: AccessType::Primary,
            recovery_mode: None,
            enforce_ulimit_nofile: true,
            column_options: LedgerColumnOptions::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessType {
    /// Primary (read/write) access; only one process can have Primary access.
    Primary,
    /// Primary (read/write) access with RocksDB automatic compaction disabled.
    PrimaryForMaintenance,
    /// Secondary (read) access; multiple processes can have Secondary access.
    /// Additionally, Secondary access can be obtained while another process
    /// already has Primary access.
    Secondary,
}

#[derive(Debug, Clone)]
pub enum BlockstoreRecoveryMode {
    TolerateCorruptedTailRecords,
    AbsoluteConsistency,
    PointInTime,
    SkipAnyCorruptedRecord,
}

impl From<&str> for BlockstoreRecoveryMode {
    fn from(string: &str) -> Self {
        match string {
            "tolerate_corrupted_tail_records" => {
                BlockstoreRecoveryMode::TolerateCorruptedTailRecords
            }
            "absolute_consistency" => BlockstoreRecoveryMode::AbsoluteConsistency,
            "point_in_time" => BlockstoreRecoveryMode::PointInTime,
            "skip_any_corrupted_record" => BlockstoreRecoveryMode::SkipAnyCorruptedRecord,
            bad_mode => panic!("Invalid recovery mode: {}", bad_mode),
        }
    }
}

impl From<BlockstoreRecoveryMode> for DBRecoveryMode {
    fn from(brm: BlockstoreRecoveryMode) -> Self {
        match brm {
            BlockstoreRecoveryMode::TolerateCorruptedTailRecords => {
                DBRecoveryMode::TolerateCorruptedTailRecords
            }
            BlockstoreRecoveryMode::AbsoluteConsistency => DBRecoveryMode::AbsoluteConsistency,
            BlockstoreRecoveryMode::PointInTime => DBRecoveryMode::PointInTime,
            BlockstoreRecoveryMode::SkipAnyCorruptedRecord => {
                DBRecoveryMode::SkipAnyCorruptedRecord
            }
        }
    }
}

/// Options for LedgerColumn.
/// Each field might also be used as a tag that supports group-by operation when
/// reporting metrics.
#[derive(Debug, Clone)]
pub struct LedgerColumnOptions {
    // Determine how to store both data and coding shreds. Default: RocksLevel.
    pub shred_storage_type: ShredStorageType,

    // Determine the way to compress column families which are eligible for
    // compression.
    pub compression_type: BlockstoreCompressionType,

    // Control how often RocksDB read/write performance samples are collected.
    // If the value is greater than 0, then RocksDB read/write perf sample
    // will be collected once for every `rocks_perf_sample_interval` ops.
    pub rocks_perf_sample_interval: usize,
}

impl Default for LedgerColumnOptions {
    fn default() -> Self {
        Self {
            shred_storage_type: ShredStorageType::RocksLevel,
            compression_type: BlockstoreCompressionType::default(),
            rocks_perf_sample_interval: 0,
        }
    }
}

impl LedgerColumnOptions {
    pub fn get_storage_type_string(&self) -> &'static str {
        match self.shred_storage_type {
            ShredStorageType::RocksLevel => "rocks_level",
            ShredStorageType::RocksFifo(_) => "rocks_fifo",
        }
    }

    pub fn get_compression_type_string(&self) -> &'static str {
        match self.compression_type {
            BlockstoreCompressionType::None => "None",
            BlockstoreCompressionType::Snappy => "Snappy",
            BlockstoreCompressionType::Lz4 => "Lz4",
            BlockstoreCompressionType::Zlib => "Zlib",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ShredStorageType {
    // Stores shreds under RocksDB's default compaction (level).
    RocksLevel,
    // (Experimental) Stores shreds under RocksDB's FIFO compaction which
    // allows ledger store to reclaim storage more efficiently with
    // lower I/O overhead.
    RocksFifo(BlockstoreRocksFifoOptions),
}

impl Default for ShredStorageType {
    fn default() -> Self {
        Self::RocksLevel
    }
}

#[derive(Debug, Clone)]
pub struct BlockstoreRocksFifoOptions {
    // The maximum storage size for storing data shreds in column family
    // [`cf::DataShred`].  Typically, data shreds contribute around 25% of the
    // ledger store storage size if the RPC service is enabled, or 50% if RPC
    // service is not enabled.
    //
    // Note that this number must be greater than FIFO_WRITE_BUFFER_SIZE
    // otherwise we won't be able to write any file.  If not, the blockstore
    // will panic.
    pub shred_data_cf_size: u64,
    // The maximum storage size for storing coding shreds in column family
    // [`cf::CodeShred`].  Typically, coding shreds contribute around 20% of the
    // ledger store storage size if the RPC service is enabled, or 40% if RPC
    // service is not enabled.
    //
    // Note that this number must be greater than FIFO_WRITE_BUFFER_SIZE
    // otherwise we won't be able to write any file.  If not, the blockstore
    // will panic.
    pub shred_code_cf_size: u64,
}

// Maximum size of cf::DataShred.  Used when `shred_storage_type`
// is set to ShredStorageType::RocksFifo.  The default value is set
// to 125GB, assuming 500GB total storage for ledger and 25% is
// used by data shreds.
const DEFAULT_FIFO_COMPACTION_DATA_CF_SIZE: u64 = 125 * 1024 * 1024 * 1024;
// Maximum size of cf::CodeShred.  Used when `shred_storage_type`
// is set to ShredStorageType::RocksFifo.  The default value is set
// to 100GB, assuming 500GB total storage for ledger and 20% is
// used by coding shreds.
const DEFAULT_FIFO_COMPACTION_CODING_CF_SIZE: u64 = 100 * 1024 * 1024 * 1024;

impl Default for BlockstoreRocksFifoOptions {
    fn default() -> Self {
        Self {
            // Maximum size of cf::ShredData.
            shred_data_cf_size: DEFAULT_FIFO_COMPACTION_DATA_CF_SIZE,
            // Maximum size of cf::ShredCode.
            shred_code_cf_size: DEFAULT_FIFO_COMPACTION_CODING_CF_SIZE,
        }
    }
}

#[derive(Debug, Clone)]
pub enum BlockstoreCompressionType {
    None,
    Snappy,
    Lz4,
    Zlib,
}

impl Default for BlockstoreCompressionType {
    fn default() -> Self {
        Self::None
    }
}

impl BlockstoreCompressionType {
    pub(crate) fn to_rocksdb_compression_type(&self) -> RocksCompressionType {
        match self {
            Self::None => RocksCompressionType::None,
            Self::Snappy => RocksCompressionType::Snappy,
            Self::Lz4 => RocksCompressionType::Lz4,
            Self::Zlib => RocksCompressionType::Zlib,
        }
    }
}
