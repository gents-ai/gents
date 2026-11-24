//! Read access to every option, for a caller that holds an [`Options`] it
//! did not build itself.
//!
//! Prefixed `get_` because the builder method of the same option already owns
//! the bare name. Every option has a getter: the table test below names each
//! field of [`Options`], so adding a field without a getter fails to
//! compile.

use super::*;

macro_rules! copy_getters {
    ($($get:ident => $field:ident: $ty:ty),* $(,)?) => {
        impl Options {
            $(
                #[doc = concat!("The value of [`Options::", stringify!($field), "`].")]
                #[must_use]
                pub fn $get(&self) -> $ty {
                    self.$field
                }
            )*
        }
    };
}

/// Getters for the options that hold an optional shared handle: the handle
/// as it was set, or `None` when the option is unset.
macro_rules! shared_getters {
    ($($get:ident => $field:ident: $ty:ty),* $(,)?) => {
        impl Options {
            $(
                #[doc = concat!("The value of [`Options::", stringify!($field), "`].")]
                #[must_use]
                pub fn $get(&self) -> Option<&Arc<$ty>> {
                    self.$field.as_ref()
                }
            )*
        }
    };
}

copy_getters! {
    get_write_buffer_size => write_buffer_size: usize,
    get_arena_profile => arena_profile: ArenaProfile,
    get_block_size => block_size: usize,
    get_block_cache_size => block_cache_size: usize,
    get_block_cache_num_shard_bits => block_cache_num_shard_bits: u32,
    get_strict_capacity_limit => strict_capacity_limit: bool,
    get_bloom_bits_per_key => bloom_bits_per_key: usize,
    get_compression => compression: CompressionType,
    get_l0_compaction_trigger => l0_compaction_trigger: usize,
    get_level_base_bytes => level_base_bytes: u64,
    get_level_size_multiplier => level_size_multiplier: u64,
    get_target_file_size => target_file_size: u64,
    get_durability => durability: DurabilityMode,
    get_atomic_flush => atomic_flush: bool,
    get_level0_slowdown_writes_trigger => level0_slowdown_writes_trigger: usize,
    get_level0_stop_writes_trigger => level0_stop_writes_trigger: usize,
    get_soft_pending_compaction_bytes_limit => soft_pending_compaction_bytes_limit: u64,
    get_hard_pending_compaction_bytes_limit => hard_pending_compaction_bytes_limit: u64,
    get_max_write_buffer_number => max_write_buffer_number: usize,
    get_compaction_style => compaction_style: CompactionStyle,
    get_fifo_compaction_options => fifo_compaction_options: FifoCompactionOptions,
    get_universal_compaction_options => universal_compaction_options: UniversalCompactionOptions,
    get_max_background_compactions => max_background_compactions: usize,
    get_log_writeback => log_writeback: bool,
    get_max_subcompactions => max_subcompactions: usize,
    get_evict_compaction_data_from_page_cache => evict_compaction_data_from_page_cache: bool,
    get_partitioned_index => partitioned_index: bool,
    get_cache_index_and_filter_blocks => cache_index_and_filter_blocks: bool,
    get_metadata_block_size => metadata_block_size: usize,
    get_read_only => read_only: bool,
    get_max_key_size => max_key_size: usize,
    get_max_value_size => max_value_size: usize,
    get_transaction_keys_inline => transaction_keys_inline: usize,
    get_max_open_files => max_open_files: usize,
}

shared_getters! {
    get_compaction_filter => compaction_filter: dyn CompactionFilter,
    get_prefix_extractor => prefix_extractor: dyn PrefixExtractor,
    get_merge_operator => merge_operator: dyn MergeOperator,
    get_transaction_hooks => transaction_hooks: dyn crate::TransactionHooks,
    get_key_provider => key_provider: dyn crate::KeyProvider,
    get_statistics => statistics: crate::Statistics,
    get_rate_limiter => rate_limiter: dyn crate::RateLimiter,
}

impl Options {
    /// The value of [`Options::inline_compaction`] as the database takes
    /// it: what was set, or, when nothing was, on for a database with no
    /// compaction worker ([`Options::max_background_compactions`] `0`) and
    /// off for one that has a worker.
    #[must_use]
    pub fn get_inline_compaction(&self) -> bool {
        self.inline_compaction_resolved()
    }

    /// The value of [`Options::compression_per_level`]: the compression of
    /// each level, shallowest first, or `None` when every level uses
    /// [`Options::compression`].
    #[must_use]
    pub fn get_compression_per_level(&self) -> Option<&[CompressionType]> {
        self.compression_per_level.as_deref()
    }

    /// The value of [`Options::listeners`], in the order they were set and
    /// added ([`Options::add_listener`]).
    #[must_use]
    pub fn get_listeners(&self) -> &[Arc<dyn crate::EventListener>] {
        &self.listeners
    }

    /// The value of [`Options::env`].
    #[must_use]
    pub fn get_env(&self) -> &Arc<dyn crate::env::Env> {
        &self.env
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_limiter::TokenBucketRateLimiter;

    struct KeepAll;

    impl CompactionFilter for KeepAll {
        fn filter(&self, _level: usize, _key: &[u8], _value: &[u8]) -> CompactionDecision {
            CompactionDecision::Keep
        }

        fn name(&self) -> &'static str {
            "keep-all"
        }
    }

    struct Declines;

    impl MergeOperator for Declines {
        fn full_merge(&self, _: &[u8], _: Option<&[u8]>, _: &[&[u8]]) -> Option<Vec<u8>> {
            None
        }

        fn name(&self) -> &'static str {
            "declines"
        }
    }

    struct Quiet;

    impl crate::EventListener for Quiet {}

    impl crate::TransactionHooks for Quiet {}

    struct OneKey;

    impl crate::KeyProvider for OneKey {
        fn current(&self) -> crate::KeyId {
            crate::KeyId(7)
        }

        fn key(&self, id: crate::KeyId) -> Option<crate::KeyMaterial> {
            (id == self.current()).then(|| crate::KeyMaterial::new([7; 32]))
        }
    }

    /// One row for every field of `Options`: the builder call that moves the
    /// field off its default, and what its getter then reads.
    ///
    /// The fields are named in a pattern with no `..`, so a field added to
    /// `Options` without a row here does not compile. A row's getter must
    /// read the default as not set and the built value as set, so a getter
    /// that reads another field, or a constant, fails.
    macro_rules! every_option_is_read_back {
        ($($field:ident: $set:expr => $reads_it:expr),* $(,)?) => {{
            let Options { $($field: _),* } = Options::default();
            $({
                let set: fn(Options) -> Options = $set;
                let reads_it: fn(&Options) -> bool = $reads_it;
                assert!(
                    !reads_it(&Options::default()),
                    "{}: the default already reads as set",
                    stringify!($field)
                );
                assert!(
                    reads_it(&set(Options::default())),
                    "{}: the getter does not read what the builder set",
                    stringify!($field)
                );
            })*
        }};
    }

    #[test]
    fn every_option_has_a_getter_that_reads_what_its_builder_set() {
        every_option_is_read_back! {
            write_buffer_size: |o| o.write_buffer_size(4096)
                => |o| o.get_write_buffer_size() == 4096,
            arena_profile: |o| o.arena_profile(ArenaProfile::EMBEDDED)
                => |o| o.get_arena_profile() == ArenaProfile::EMBEDDED,
            block_size: |o| o.block_size(1024) => |o| o.get_block_size() == 1024,
            block_cache_size: |o| o.block_cache_size(1024)
                => |o| o.get_block_cache_size() == 1024,
            block_cache_num_shard_bits: |o| o.block_cache_num_shard_bits(2)
                => |o| o.get_block_cache_num_shard_bits() == 2,
            strict_capacity_limit: |o| o.strict_capacity_limit(true)
                => |o| o.get_strict_capacity_limit(),
            bloom_bits_per_key: |o| o.bloom_bits_per_key(3)
                => |o| o.get_bloom_bits_per_key() == 3,
            compression: |o| o.compression(CompressionType::Snappy)
                => |o| o.get_compression() == CompressionType::Snappy,
            compression_per_level: |o| o.compression_per_level(Some(vec![CompressionType::None]))
                => |o| o.get_compression_per_level() == Some(&[CompressionType::None][..]),
            l0_compaction_trigger: |o| o.l0_compaction_trigger(9)
                => |o| o.get_l0_compaction_trigger() == 9,
            level_base_bytes: |o| o.level_base_bytes(1 << 20)
                => |o| o.get_level_base_bytes() == 1 << 20,
            level_size_multiplier: |o| o.level_size_multiplier(7)
                => |o| o.get_level_size_multiplier() == 7,
            target_file_size: |o| o.target_file_size(1 << 20)
                => |o| o.get_target_file_size() == 1 << 20,
            durability: |o| o.durability(DurabilityMode::Immediate)
                => |o| o.get_durability() == DurabilityMode::Immediate,
            compaction_filter: |o| o.compaction_filter(Some(Arc::new(KeepAll)))
                => |o| o.get_compaction_filter().is_some_and(|f| f.name() == "keep-all"),
            prefix_extractor: |o| o.prefix_extractor(Some(Arc::new(FixedLengthPrefix(2))))
                => |o| o.get_prefix_extractor().is_some_and(|p| p.name() == "FixedLengthPrefix(2)"),
            merge_operator: |o| o.merge_operator(Some(Arc::new(Declines)))
                => |o| o.get_merge_operator().is_some_and(|m| m.name() == "declines"),
            atomic_flush: |o| o.atomic_flush(true) => |o| o.get_atomic_flush(),
            listeners: |o| o.listeners(vec![Arc::new(Quiet)])
                => |o| o.get_listeners().len() == 1,
            transaction_hooks: |o| o.transaction_hooks(Arc::new(Quiet))
                => |o| o.get_transaction_hooks().is_some(),
            key_provider: |o| o.key_provider(Arc::new(OneKey))
                => |o| o.get_key_provider().is_some_and(|p| p.current() == crate::KeyId(7)),
            statistics: |o| o.statistics(Some(Arc::new(crate::Statistics::new())))
                => |o| o.get_statistics().is_some(),
            rate_limiter: |o| {
                o.rate_limiter(Some(Arc::new(TokenBucketRateLimiter::new(1 << 20, 1 << 20))))
            } => |o| o.get_rate_limiter().is_some(),
            level0_slowdown_writes_trigger: |o| o.level0_slowdown_writes_trigger(5)
                => |o| o.get_level0_slowdown_writes_trigger() == 5,
            level0_stop_writes_trigger: |o| o.level0_stop_writes_trigger(6)
                => |o| o.get_level0_stop_writes_trigger() == 6,
            soft_pending_compaction_bytes_limit: |o| o.soft_pending_compaction_bytes_limit(1 << 20)
                => |o| o.get_soft_pending_compaction_bytes_limit() == 1 << 20,
            hard_pending_compaction_bytes_limit: |o| o.hard_pending_compaction_bytes_limit(1 << 21)
                => |o| o.get_hard_pending_compaction_bytes_limit() == 1 << 21,
            max_write_buffer_number: |o| o.max_write_buffer_number(3)
                => |o| o.get_max_write_buffer_number() == 3,
            compaction_style: |o| o.compaction_style(CompactionStyle::Universal)
                => |o| o.get_compaction_style() == CompactionStyle::Universal,
            fifo_compaction_options: |o| {
                o.fifo_compaction_options(FifoCompactionOptions { max_table_files_size: 1 << 20 })
            } => |o| o.get_fifo_compaction_options().max_table_files_size == 1 << 20,
            universal_compaction_options: |o| {
                o.universal_compaction_options(UniversalCompactionOptions {
                    size_ratio: 5,
                    ..UniversalCompactionOptions::default()
                })
            } => |o| o.get_universal_compaction_options().size_ratio == 5,
            max_background_compactions: |o| o.max_background_compactions(4)
                => |o| o.get_max_background_compactions() == 4,
            log_writeback: |o| o.log_writeback(true)
                => |o| o.get_log_writeback(),
            // The default follows the target's worker count, so the row
            // sets the other answer, whichever that is.
            inline_compaction: |o| o.inline_compaction(DEFAULT_MAX_BACKGROUND_COMPACTIONS > 0)
                => |o| o.get_inline_compaction() == (DEFAULT_MAX_BACKGROUND_COMPACTIONS > 0),
            max_subcompactions: |o| o.max_subcompactions(2)
                => |o| o.get_max_subcompactions() == 2,
            evict_compaction_data_from_page_cache: |o| o.evict_compaction_data_from_page_cache(true)
                => |o| o.get_evict_compaction_data_from_page_cache(),
            partitioned_index: |o| o.partitioned_index(true) => |o| o.get_partitioned_index(),
            cache_index_and_filter_blocks: |o| o.cache_index_and_filter_blocks(true)
                => |o| o.get_cache_index_and_filter_blocks(),
            metadata_block_size: |o| o.metadata_block_size(512)
                => |o| o.get_metadata_block_size() == 512,
            read_only: |o| o.read_only(true) => |o| o.get_read_only(),
            max_key_size: |o| o.max_key_size(1024) => |o| o.get_max_key_size() == 1024,
            max_value_size: |o| o.max_value_size(2048) => |o| o.get_max_value_size() == 2048,
            transaction_keys_inline: |o| o.transaction_keys_inline(3)
                => |o| o.get_transaction_keys_inline() == 3,
            env: |o| o.env(Arc::new(crate::MemEnv::new()))
                => |o| !o.get_env().capabilities().threads,
            max_open_files: |o| o.max_open_files(64) => |o| o.get_max_open_files() == 64,
        }
    }

    /// The inline-compaction getter reads the switch as the database takes
    /// it: left unset it follows the worker count, and said explicitly it is
    /// what was said.
    #[test]
    fn the_inline_compaction_getter_reads_what_the_database_takes() {
        let workers = |n| Options::default().max_background_compactions(n);
        assert!(workers(0).get_inline_compaction(), "no worker, unset");
        assert!(!workers(1).get_inline_compaction(), "a worker, unset");
        for n in [0, 1] {
            assert!(!workers(n).inline_compaction(false).get_inline_compaction());
            assert!(workers(n).inline_compaction(true).get_inline_compaction());
        }
        assert_eq!(
            Options::default().get_inline_compaction(),
            DEFAULT_MAX_BACKGROUND_COMPACTIONS == 0
        );
    }

    /// A listener is added after the ones already set, and the getter hands
    /// them back in that order.
    #[test]
    fn a_listener_is_added_after_the_ones_already_set() {
        let (first, second, third): (
            Arc<dyn crate::EventListener>,
            Arc<dyn crate::EventListener>,
            Arc<dyn crate::EventListener>,
        ) = (Arc::new(Quiet), Arc::new(Quiet), Arc::new(Quiet));
        let options = Options::default()
            .listeners(vec![Arc::clone(&first)])
            .add_listener(Arc::clone(&second))
            .add_listener(Arc::clone(&third));
        let set = options.get_listeners();
        assert_eq!(set.len(), 3);
        for (got, want) in set.iter().zip([&first, &second, &third]) {
            assert!(Arc::ptr_eq(got, want));
        }
        // On options with none set, it is the first.
        let alone = Options::default().add_listener(Arc::clone(&first));
        assert!(Arc::ptr_eq(&alone.get_listeners()[0], &first));
    }
}
