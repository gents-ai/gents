//! Portable crawl policy and inventory parsers.
pub mod sitemap;
pub use sitemap::{
    ROBOTS_PRODUCT_TOKEN, Robots, SitemapEntry, maybe_gunzip, origin_of, parse_sitemap,
    robots_for_origin,
};
