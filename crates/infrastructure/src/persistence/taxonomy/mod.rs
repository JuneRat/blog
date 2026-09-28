//! 标签、分类与系列适配器；各资源内部保留原有事务和锁顺序。
mod categories;
mod series;
mod tags;

pub use categories::{PostgresCategoryRepository, PostgresPublishedCategoryQuery};
pub use series::{PostgresPublishedSeriesQuery, PostgresSeriesRepository};
pub use tags::{PostgresPublishedTagQuery, PostgresTagRepository};
