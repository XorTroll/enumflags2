#![recursion_limit = "2048"]
extern crate proc_macro;
#[macro_use]
extern crate quote;

use syn::{Data, Ident, DeriveInput, DataEnum, spanned::Spanned};
use proc_macro2::TokenStream;
use proc_macro2::Span;

/// Shorthand for a quoted `compile_error!`.
macro_rules! error {
    ($span:expr => $($x:tt)*) => {
        quote_spanned!($span => compile_error!($($x)*);)
    };
    ($($x:tt)*) => {
        quote!(compile_error!($($x)*);)
    };
}

#[proc_macro_derive(BitFlags_internal)]
pub fn derive_enum_flags(input: proc_macro::TokenStream)
    -> proc_macro::TokenStream
{
    let ast: DeriveInput = syn::parse(input).unwrap();

    match ast.data {
        Data::Enum(ref data) => {
            gen_enumflags(&ast.ident, &ast, data)
                .unwrap_or_else(|err| err.to_compile_error())
                .into()
        }
        Data::Struct(ref data) => {
            error!(data.struct_token.span => "BitFlags can only be derived on enums").into()
        }
        Data::Union(ref data) => {
            error!(data.union_token.span => "BitFlags can only be derived on enums").into()
        }
    }
}

/// Try to evaluate the expression given.
fn fold_expr(expr: &syn::Expr) -> Result<Option<u64>, syn::Error> {
    /// Recurse, but bubble-up both kinds of errors.
    /// (I miss my monad transformers)
    macro_rules! fold_expr {
        ($($x:tt)*) => {
            match fold_expr($($x)*)? {
                Some(x) => x,
                None => return Ok(None),
            }
        }
    }

    use syn::Expr;
    match expr {
        Expr::Lit(ref expr_lit) => {
            match expr_lit.lit {
                syn::Lit::Int(ref lit_int) => {
                    Ok(Some(lit_int.base10_parse()
                        .map_err(|_| syn::Error::new_spanned(lit_int,
                                "Integer literal out of range"))?))
                }
                _ => Ok(None),
            }
        },
        Expr::Binary(ref expr_binary) => {
            let l = fold_expr!(&expr_binary.left);
            let r = fold_expr!(&expr_binary.right);
            match &expr_binary.op {
                syn::BinOp::Shl(_) => Ok(Some(l << r)),
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// Given a list of attributes, find the `repr`, if any, and return the integer
/// type specified.
fn extract_repr(attrs: &[syn::Attribute])
    -> Result<Option<syn::Ident>, syn::Error>
{
    use syn::{Meta, NestedMeta};
    attrs.iter()
        .find_map(|attr| {
            match attr.parse_meta() {
                Err(why) => {
                    Some(Err(syn::Error::new_spanned(attr,
                        format!("Couldn't parse attribute: {}", why))))
                }
                Ok(Meta::List(ref meta)) if meta.path.is_ident("repr") => {
                    meta.nested.iter()
                        .find_map(|mi| match mi {
                            NestedMeta::Meta(Meta::Path(path)) => {
                                path.get_ident().cloned()
                                    .map(Ok)
                            }
                            _ => None
                        })
                }
                Ok(_) => None
            }
        })
        .transpose()
}

/// Returns deferred checks
fn verify_flag_values<'a>(
    type_name: &Ident,
    variants: impl Iterator<Item=&'a syn::Variant>
) -> Result<TokenStream, syn::Error> {
    let mut deferred_checks: Vec<TokenStream> = vec![];
    for variant in variants {
        // I'd use matches! if not for MSRV...
        match variant.fields {
            syn::Fields::Unit => (),
            _ => return Err(syn::Error::new_spanned(&variant.fields,
                "Bitflag variants cannot contain additional data")),
        }

        let discr = variant.discriminant.as_ref()
           .ok_or_else(|| syn::Error::new_spanned(variant,
                         "Please add an explicit discriminant"))?;
        match fold_expr(&discr.1)? {
            Some(flag) => {
                if !flag.is_power_of_two() {
                    return Err(syn::Error::new_spanned(&discr.1,
                        "Flags must have exactly one set bit"));
                }
            }
            None => {
                let variant_name = &variant.ident;
                // TODO: Remove this madness when Debian ships a new compiler.
                let assertion_name = syn::Ident::new(
                    &format!("__enumflags_assertion_{}_{}",
                            type_name, variant_name),
                    Span::call_site()); // call_site because def_site is unstable

                deferred_checks.push(quote_spanned!(variant.span() =>
                    #[doc(hidden)]
                    const #assertion_name:
                        <<[(); (
                            (#type_name::#variant_name as u64).wrapping_sub(1) &
                            (#type_name::#variant_name as u64) == 0 &&
                            (#type_name::#variant_name as u64) != 0
                        ) as usize] as enumflags2::_internal::AssertionHelper>
                            ::Status as enumflags2::_internal::ExactlyOneBitSet>::X = ();
                ));
            }
        }
    }

    Ok(quote!(
        #(#deferred_checks)*
    ))
}

fn gen_enumflags(ident: &Ident, item: &DeriveInput, data: &DataEnum)
    -> Result<TokenStream, syn::Error>
{
    let span = Span::call_site();
    // for quote! interpolation
    let variants = data.variants.iter().map(|v| &v.ident);
    let variants_len = data.variants.len();
    let names = std::iter::repeat(&ident);
    let ty = extract_repr(&item.attrs)?
        .unwrap_or_else(|| Ident::new("usize", span));

    let deferred = verify_flag_values(ident, data.variants.iter())?;
    let std_path = quote_spanned!(span => ::enumflags2::_internal::core);
    let all = if variants_len == 0 {
        quote!(0)
    } else {
        let names = names.clone();
        let variants = variants.clone();
        quote!(#(#names::#variants as #ty)|*)
    };

    Ok(quote_spanned! {
        span => #deferred
            impl #std_path::ops::Not for #ident {
                type Output = ::enumflags2::BitFlags<#ident>;
                fn not(self) -> Self::Output {
                    use ::enumflags2::{BitFlags, _internal::RawBitFlags};
                    unsafe { BitFlags::new(self.bits()).not() }
                }
            }

            impl #std_path::ops::BitOr for #ident {
                type Output = ::enumflags2::BitFlags<#ident>;
                fn bitor(self, other: Self) -> Self::Output {
                    use ::enumflags2::{BitFlags, _internal::RawBitFlags};
                    unsafe { BitFlags::new(self.bits() | other.bits())}
                }
            }

            impl #std_path::ops::BitAnd for #ident {
                type Output = ::enumflags2::BitFlags<#ident>;
                fn bitand(self, other: Self) -> Self::Output {
                    use ::enumflags2::{BitFlags, _internal::RawBitFlags};
                    unsafe { BitFlags::new(self.bits() & other.bits())}
                }
            }

            impl #std_path::ops::BitXor for #ident {
                type Output = ::enumflags2::BitFlags<#ident>;
                fn bitxor(self, other: Self) -> Self::Output {
                    #std_path::convert::Into::<Self::Output>::into(self) ^ #std_path::convert::Into::<Self::Output>::into(other)
                }
            }

            impl ::enumflags2::_internal::RawBitFlags for #ident {
                type Type = #ty;

                fn all_bits() -> Self::Type {
                    // make sure it's evaluated at compile time
                    const VALUE: #ty = #all;
                    VALUE
                }

                fn bits(self) -> Self::Type {
                    self as #ty
                }

                fn flag_list() -> &'static [Self] {
                    const VARIANTS: [#ident; #variants_len] = [#(#names :: #variants),*];
                    &VARIANTS
                }

                fn bitflags_type_name() -> &'static str {
                    concat!("BitFlags<", stringify!(#ident), ">")
                }
            }

            impl ::enumflags2::RawBitFlags for #ident {}
    })
}
