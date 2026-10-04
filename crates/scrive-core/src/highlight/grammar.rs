//! [`Grammar`]: the backend-neutral grammar a [`crate::Document`] highlights with.

use super::SyntaxDef;

/// A grammar for [`crate::Document::set_syntax`], built only through its
/// `From` impls: `From<SyntaxDef>` for a syntect `.sublime-syntax` grammar.
/// Opaque, so a backend added later can't break a caller's `match`.
pub struct Grammar(pub(super) Inner);

/// The backend a [`Grammar`] selects. Visible to `highlight` alone, which
/// builds the matching cache backend from it.
pub(super) enum Inner {
    Syntect(SyntaxDef),
}

impl From<SyntaxDef> for Grammar {
    fn from(def: SyntaxDef) -> Self {
        Self(Inner::Syntect(def))
    }
}
