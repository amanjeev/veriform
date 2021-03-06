//! Custom derive support for the `Message` trait

use crate::{
    digest,
    field::{self, WireType},
};
use darling::{FromField, FromVariant};
use proc_macro2::TokenStream;
use quote::{quote, ToTokens};
use syn::{DataEnum, DataStruct, Field, Ident};
use synstructure::Structure;

/// Custom derive for `Message`
pub(crate) fn derive(s: Structure<'_>) -> TokenStream {
    match &s.ast().data {
        syn::Data::Enum(data) => DeriveEnum::derive(s, data),
        syn::Data::Struct(data) => DeriveStruct::derive(s, data),
        other => panic!("can't derive `Message` on: {:?}", other),
    }
}

/// Derive `Message` on an enum
struct DeriveEnum {
    /// Body of `Message::decode()` in-progress for an enum
    decode_body: TokenStream,

    /// Body of `Message::encode()` in-progress for an enum
    encode_body: TokenStream,

    /// Body of `Message::encoded_len()` in-progress for an enum
    encoded_len_body: TokenStream,
}

impl DeriveEnum {
    /// Derive `Message` on an enum
    // TODO(tarcieri): higher-level abstractions/implementation?
    pub fn derive(s: Structure<'_>, data: &DataEnum) -> TokenStream {
        assert_eq!(
            s.variants().len(),
            data.variants.len(),
            "enum variant count mismatch"
        );

        let mut state = Self {
            decode_body: TokenStream::new(),
            encode_body: TokenStream::new(),
            encoded_len_body: TokenStream::new(),
        };

        for (variant_info, variant) in s.variants().iter().zip(&data.variants) {
            let attrs = field::Attrs::from_variant(variant).unwrap_or_else(|e| {
                panic!("error parsing field attributes: {}", e);
            });

            state.derive_decode_match_arm(&variant.ident, &attrs);

            variant_info
                .each(|bi| encode_field(&bi.binding, &attrs))
                .to_tokens(&mut state.encode_body);

            variant_info
                .each(|bi| encoded_len_for_field(&bi.binding, &attrs))
                .to_tokens(&mut state.encoded_len_body)
        }

        state.finish(s)
    }

    /// Derive a match arm of an enum `decode` method
    fn derive_decode_match_arm(&mut self, name: &Ident, attrs: &field::Attrs) {
        let tag = attrs.tag();
        let wire_type = attrs.wire_type();

        let decode_variant = if wire_type.is_ref_type() {
            let ty = wire_type.rust_type().unwrap();
            quote! {
                let field: #ty = decoder.decode_ref(#tag, &mut input)?;
                field
                    .try_into()
                    .map(Self::#name)
                    .map_err(|_| veriform::field::WireType::Bytes.decoding_error())
            }
        } else if wire_type.is_sequence() {
            todo!();
        } else {
            quote! {
                decoder.decode(#tag, &mut input).map(Self::#name)
            }
        };

        let match_arm = quote! {
            #tag => { #decode_variant }
        };

        match_arm.to_tokens(&mut self.decode_body);
    }

    /// Finish deriving an enum
    fn finish(self, s: Structure<'_>) -> TokenStream {
        let decode_body = self.decode_body;
        let encode_body = self.encode_body;
        let encoded_len_body = self.encoded_len_body;

        s.gen_impl(quote! {
            gen impl Message for @Self {
                fn decode<D>(
                    decoder: &mut veriform::decoder::Decoder<D>,
                    mut input: &[u8]
                ) -> Result<Self, veriform::Error>
                where
                    D: veriform::digest::Digest,
                {
                    #[allow(unused_imports)]
                    use core::convert::TryInto;
                    #[allow(unused_imports)]
                    use veriform::decoder::{Decode, DecodeRef};

                    let msg = match veriform::derive_helpers::decode_tag(input)? {
                        #decode_body
                        tag => Err(veriform::derive_helpers::unknown_tag(tag))
                    }?;

                    veriform::derive_helpers::check_input_consumed(input)?;
                    Ok(msg)
                }

                fn encode<'a>(
                    &self,
                    buffer: &'a mut [u8]
                ) -> Result<&'a [u8], veriform::Error> {
                    let mut encoder = veriform::Encoder::new(buffer);

                    match self {
                        #encode_body
                    }

                    Ok(encoder.finish())
                }

                fn encoded_len(&self) -> usize {
                    match self {
                        #encoded_len_body
                    }
                }
            }
        })
    }
}

/// Derive `Message` on a struct
// TODO(tarcieri): make sure tags are in the right order and digest is the last field
struct DeriveStruct {
    /// Body of `Message::decode()` in-progress for a struct
    decode_body: TokenStream,

    /// Instantiation of the struct at the end of `Message::decode()`
    inst_body: TokenStream,

    /// Body of `Message::encode()` in-progress for a struct
    encode_body: TokenStream,

    /// Body of `Message::encoded_len()` in-progress for a struct
    encoded_len_body: TokenStream,
}

impl DeriveStruct {
    pub fn derive(s: Structure<'_>, data: &DataStruct) -> TokenStream {
        assert_eq!(s.variants().len(), 1, "expected one variant");

        let mut state = Self {
            decode_body: TokenStream::new(),
            inst_body: TokenStream::new(),
            encode_body: TokenStream::new(),
            encoded_len_body: quote!(0),
        };

        let variant = &s.variants()[0];
        let bindings = &variant.bindings();

        if bindings.len() != data.fields.len() {
            panic!(
                "unexpected number of bindings ({} vs {})",
                bindings.len(),
                data.fields.len()
            );
        }

        for (binding_info, field) in bindings.iter().zip(&data.fields) {
            for attr in &field.attrs {
                let attr_segments = &attr.path.segments;

                // ignore namespaced attributes
                if attr_segments.len() != 1 {
                    continue;
                }

                let attr_name = &attr_segments[0].ident.to_string();

                match attr_name.as_ref() {
                    "field" => state.derive_field(field, &binding_info.binding),
                    "digest" => state.derive_digest(field),
                    _ => (), // ignore other attributes
                }
            }
        }

        state.finish(&s, variant.pat())
    }

    /// Derive handling for a particular `#[field(...)]`
    fn derive_field(&mut self, field: &Field, binding: &Ident) {
        let name = parse_field_name(field);

        let attrs = field::Attrs::from_field(field).unwrap_or_else(|e| {
            panic!("error parsing field attributes: {}", e);
        });

        self.derive_decode_field(name, &attrs);

        let inst_field = quote!(#name,);
        inst_field.to_tokens(&mut self.inst_body);

        let enc_field = encode_field(binding, &attrs);
        let enc_field_with_semicolon = quote!(#enc_field;);
        enc_field_with_semicolon.to_tokens(&mut self.encode_body);

        let enc_field_len = encoded_len_for_field(binding, &attrs);
        let enc_field_len_with_plus = quote!(+ #enc_field_len);
        enc_field_len_with_plus.to_tokens(&mut self.encoded_len_body);
    }

    /// Derive a match arm of an struct `decode` method
    fn derive_decode_field(&mut self, name: &Ident, attrs: &field::Attrs) {
        let tag = attrs.tag();
        let wire_type = attrs.wire_type();

        match wire_type.rust_type() {
            Some(ty) => {
                if wire_type.is_ref_type() {
                    quote! { let #name: #ty = decoder.decode_ref(#tag, &mut input)?; }
                } else {
                    quote! { let #name: #ty = decoder.decode(#tag, &mut input)?; }
                }
            }
            None => {
                if wire_type.is_message() {
                    quote! { let #name = decoder.decode(#tag, &mut input)?; }
                } else if wire_type.is_sequence() {
                    // TODO(tarcieri): hoist more of this into a `derive_helper` function?
                    quote! {
                        let #name = veriform::derive_helpers::decode_message_seq(
                            decoder,
                            #tag,
                            &mut input
                        )?;
                    }
                } else {
                    unreachable!();
                }
            }
        }
        .to_tokens(&mut self.decode_body);
    }

    /// Derive handling for a `#[digest(...)]` member of a struct
    fn derive_digest(&mut self, field: &Field) {
        let name = parse_field_name(field);

        let attrs = digest::Attrs::from_field(field).unwrap_or_else(|e| {
            panic!("error parsing digest attributes: {}", e);
        });

        // TODO(tarcieri): support additional algorithms?
        match attrs.alg() {
            digest::Algorithm::Sha256 => self.derive_sha256_digest(&name),
        }
    }

    /// Derive computing a SHA-256 digest of a message
    fn derive_sha256_digest(&mut self, name: &Ident) {
        let fill_digest = quote! {
            let mut #name = veriform::Sha256Digest::default();
            decoder.fill_digest(&mut #name)?;
        };

        fill_digest.to_tokens(&mut self.decode_body);

        let inst_field = quote!(#name: Some(#name),);
        inst_field.to_tokens(&mut self.inst_body);
    }

    /// Finish deriving a struct
    fn finish(self, s: &Structure<'_>, pattern: TokenStream) -> TokenStream {
        let decode_body = self.decode_body;
        let inst_body = self.inst_body;
        let encode_body = self.encode_body;
        let encoded_len_body = self.encoded_len_body;

        s.gen_impl(quote! {
            gen impl Message for @Self {
                fn decode<D>(
                    decoder: &mut veriform::decoder::Decoder<D>,
                    mut input: &[u8]
                ) -> Result<Self, veriform::Error>
                where
                    D: veriform::digest::Digest,
                {
                    #[allow(unused_imports)]
                    use veriform::decoder::{Decode, DecodeRef};

                    #decode_body

                    Ok(Self { #inst_body })
                }

                fn encode<'a>(
                    &self,
                    buffer: &'a mut [u8]
                ) -> Result<&'a [u8], veriform::Error> {
                    let mut encoder = veriform::Encoder::new(buffer);

                    match self {
                        #pattern => { #encode_body }
                    }

                    Ok(encoder.finish())
                }

                fn encoded_len(&self) -> usize {
                    match self {
                        #pattern => { #encoded_len_body }
                    }
                }
            }
        })
    }
}

/// Parse the name of a field
fn parse_field_name(field: &Field) -> &Ident {
    field
        .ident
        .as_ref()
        .unwrap_or_else(|| panic!("no name on struct field (e.g. tuple structs unsupported)"))
}

/// Encode a field of a message
fn encode_field(binding: &Ident, attrs: &field::Attrs) -> TokenStream {
    let tag = attrs.tag();
    let critical = attrs.is_critical();

    match attrs.wire_type() {
        WireType::Bool => todo!(),
        WireType::UInt64 => quote! { encoder.uint64(#tag, #critical, *#binding)? },
        WireType::SInt64 => quote! { encoder.sint64(#tag, #critical, *#binding)? },
        WireType::Bytes => quote! { encoder.bytes(#tag, #critical, #binding)? },
        WireType::String => quote! { encoder.string(#tag, #critical, #binding)? },
        WireType::Message => quote! { encoder.message(#tag, #critical, #binding)? },
        WireType::Sequence => quote! {
            // TODO(tarcieri): support other types of sequences besides messages
            veriform::derive_helpers::encode_message_seq(&mut encoder, #tag, #critical, #binding)?;
        },
    }
}

/// Compute the encoded length of a field
fn encoded_len_for_field(binding: &Ident, attrs: &field::Attrs) -> TokenStream {
    let tag = attrs.tag();

    match attrs.wire_type() {
        WireType::Bool => todo!(),
        WireType::UInt64 => quote! { veriform::field::length::uint64(#tag, *#binding) },
        WireType::SInt64 => quote! { veriform::field::length::sint64(#tag, *#binding) },
        WireType::Bytes => quote! { veriform::field::length::bytes(#tag, #binding) },
        WireType::String => quote! { veriform::field::length::string(#tag, #binding) },
        WireType::Message => quote! { veriform::field::length::message(#tag, #binding) },
        WireType::Sequence => quote! {
            // TODO(tarcieri): support other types of sequences besides messages
            veriform::field::length::message_seq(
                #tag,
                #binding.iter().map(|elem| elem as &dyn veriform::Message)
            )
        },
    }
}
