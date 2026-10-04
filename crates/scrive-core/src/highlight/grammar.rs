//! [`Grammar`]: the backend-neutral grammar a [`crate::Document`] highlights with.

#[cfg(feature = "syntect")]
use super::SyntaxDef;
#[cfg(feature = "tree-sitter")]
use super::TreeSitterDef;

/// A grammar for [`crate::Document::set_syntax`], built only through its
/// `From` impls: `From<SyntaxDef>` (feature `syntect`) for a `.sublime-syntax`
/// grammar, and `From<TreeSitterDef>` (feature `tree-sitter`) for a
/// tree-sitter grammar. With neither feature it has no value at all. Opaque,
/// so a backend added later can't break a caller's `match`.
pub struct Grammar(pub(super) Inner);

/// The backend a [`Grammar`] selects. Visible to `highlight` alone, which
/// builds the matching cache backend from it.
pub(super) enum Inner {
    #[cfg(feature = "syntect")]
    Syntect(SyntaxDef),
    #[cfg(feature = "tree-sitter")]
    TreeSitter(TreeSitterDef),
}

#[cfg(feature = "syntect")]
impl From<SyntaxDef> for Grammar {
    fn from(def: SyntaxDef) -> Self {
        Self(Inner::Syntect(def))
    }
}

#[cfg(feature = "tree-sitter")]
impl From<TreeSitterDef> for Grammar {
    fn from(def: TreeSitterDef) -> Self {
        Self(Inner::TreeSitter(def))
    }
}
