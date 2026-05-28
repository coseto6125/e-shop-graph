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
    /// A purchasable variant of a Product (color/size/style), each with its own
    /// price/sku/inventory. Platform stores (Shopify-like, easy.co) model this
    /// as `product.variants[]`; it plays the role schema.org gives `Offer`.
    /// Appended at the END for rkyv discriminant stability.
    Variant,
}

impl NodeKind {
    /// Parse a schema.org type label (as written in Cypher / JSON-LD `@type`).
    pub fn from_label(s: &str) -> Option<Self> {
        Some(match s {
            "Product" => Self::Product,
            "Offer" => Self::Offer,
            "Brand" => Self::Brand,
            "Organization" => Self::Organization,
            "Review" => Self::Review,
            "AggregateRating" => Self::AggregateRating,
            "Category" => Self::Category,
            "Person" => Self::Person,
            "Variant" => Self::Variant,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Product => "Product",
            Self::Offer => "Offer",
            Self::Brand => "Brand",
            Self::Organization => "Organization",
            Self::Review => "Review",
            Self::AggregateRating => "AggregateRating",
            Self::Category => "Category",
            Self::Person => "Person",
            Self::Variant => "Variant",
        }
    }
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
    /// Product -> Variant. Appended at the END (rkyv discriminant stability).
    HasVariant,
    /// Category -> Category (breadcrumb parent chain, schema.org `broader`).
    /// `Product -[:Category]-> Category` reuses `Category`; this only models the
    /// parent edge BETWEEN categories. Appended at the END (rkyv stability).
    BroaderCategory,
}

impl RelType {
    /// Parse a relation type label as written in Cypher (`-[:Brand]->`).
    pub fn from_label(s: &str) -> Option<Self> {
        Some(match s {
            "Offers" => Self::Offers,
            "Brand" => Self::Brand,
            "Manufacturer" => Self::Manufacturer,
            "Review" => Self::Review,
            "AggregateRating" => Self::AggregateRating,
            "IsVariantOf" => Self::IsVariantOf,
            "Category" => Self::Category,
            "Author" => Self::Author,
            "HasVariant" => Self::HasVariant,
            "BroaderCategory" => Self::BroaderCategory,
            _ => return None,
        })
    }
}
