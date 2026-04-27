//! Proc-macro crate for `#[derive(PiiCodec)]`.
//!
//! Generates a `{Name}PiiCodec` struct and a stub [`PiiEventCodec`] implementation
//! from an annotated event enum.  The stub panics at runtime until the full
//! implementation is generated in later phases.
//!
//! # Usage
//!
//! ```rust,ignore
//! use cqrs_es_crypto::PiiCodec;
//!
//! #[derive(PiiCodec)]
//! enum MyEvent {
//!     #[pii(event_type = "SensitiveEvent")]
//!     SensitiveEvent {
//!         #[pii(subject)]  subject_id: uuid::Uuid,
//!         #[pii(plaintext)] tag: String,
//!         #[pii(secret)]   secret: String,
//!     },
//!     PlainEvent { data: String },
//! }
//! // Generates: pub struct MyEventPiiCodec;
//! // + stub impl PiiEventCodec for MyEventPiiCodec { ... }
//! ```

mod model;
mod parse;

use zyn::ToTokens as _;

/// Derives a [`PiiEventCodec`](::cqrs_es_crypto::PiiEventCodec) implementation
/// from an annotated event enum.
///
/// See the [crate-level documentation](self) for the annotation syntax.
#[zyn::derive("PiiCodec", attributes(pii))]
fn pii_codec(
    #[zyn(input)] ident: zyn::Extract<zyn::syn::Ident>,
    #[zyn(input)] variants: zyn::Variants,
) -> zyn::TokenStream {
    let codec_ident = zyn::format_ident!("{}PiiCodec", *ident);

    // Parse and validate `#[pii(...)]` annotations.  Any error is surfaced as
    // a `compile_error!` at the call site with an accurate source span.
    match parse::parse_pii_variants(&variants) {
        Ok(_) => {}
        Err(e) => return e.into_compile_error().into(),
    }

    // Phase 1: emit a compilable stub.
    // The four trait methods are replaced with full implementations in Phase 2+.
    zyn::zyn! {
        pub struct {{ codec_ident }};

        impl ::cqrs_es_crypto::PiiEventCodec for {{ codec_ident }} {
            fn classify(
                &self,
                _event: &::cqrs_es::persist::SerializedEvent,
            ) -> ::core::option::Option<::cqrs_es_crypto::PiiFields> {
                ::core::todo!("PiiCodec implementation not yet complete")
            }

            fn extract_encrypted(
                &self,
                _event: &::cqrs_es::persist::SerializedEvent,
            ) -> ::core::option::Option<::cqrs_es_crypto::EncryptedPiiExtract> {
                ::core::todo!()
            }

            fn reconstruct(
                &self,
                _event: &::cqrs_es::persist::SerializedEvent,
                _plaintext_pii: &::serde_json::Value,
            ) -> ::core::result::Result<
                ::serde_json::Value,
                ::std::boxed::Box<dyn ::std::error::Error + Send + Sync>,
            > {
                ::core::todo!()
            }

            fn redact(
                &self,
                _event: &::cqrs_es::persist::SerializedEvent,
            ) -> ::core::result::Result<
                ::serde_json::Value,
                ::std::boxed::Box<dyn ::std::error::Error + Send + Sync>,
            > {
                ::core::todo!()
            }
        }
    }
    .to_token_stream()
}
