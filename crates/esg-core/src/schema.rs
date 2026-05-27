//! schema.org e-commerce subset. Closed enums (not an open ontology) so the
//! graph stays a specialized zero-copy structure — discriminant dispatch beats
//! generic type lookup, and e-commerce needs only a handful of types.
//!
//! Discriminant stability: append new variants at the END only. rkyv encodes
//! enums by discriminant; reordering invalidates existing `graph.bin` files.

use rkyv::{Archive, Deserialize, Serialize};

/// schema.org Types we model. Maps to JSON-LD `@type`.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[rkyv(derive(Debug, PartialEq, Eq))]
#[repr(u8)]
pub enum NodeKind {
    Product,
    Offer,
    Brand,
    Organization,
    Review,
    AggregateRating,
    Category,
    Person,
}

/// schema.org properties that become graph edges. Maps to JSON-LD property
/// keys (e.g. `Product.offers`, `Product.brand`).
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[rkyv(derive(Debug, PartialEq, Eq))]
#[repr(u8)]
pub enum RelType {
    /// Product -> Offer
    Offers,
    /// Product -> Brand
    Brand,
    /// Product -> Organization
    Manufacturer,
    /// Product -> Review
    Review,
    /// Product -> AggregateRating
    AggregateRating,
    /// Product -> Product (variant grouping, schema.org isVariantOf)
    IsVariantOf,
    /// Product -> Category
    Category,
    /// Review -> Person
    Author,
}
