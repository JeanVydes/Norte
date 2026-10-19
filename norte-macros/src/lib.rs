#![forbid(unsafe_code)]
#![doc = "Attribute macros for `Norte` tool declarations."]

use proc_macro::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Ident, ItemStruct, LitStr, Path, Token, parse_macro_input};

/// The `name = "..."`, `description = "..."` and `crate = <path>` arguments of
/// the [`tool`] attribute, in any order.
struct ToolMetadataArgs {
    name: LitStr,
    description: LitStr,
    /// The crate that defines `ToolMetadata`; `None` keeps the attribute's
    /// default anchor.
    anchor: Option<Path>,
}

impl Parse for ToolMetadataArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name: Option<LitStr> = None;
        let mut description: Option<LitStr> = None;
        let mut anchor: Option<Path> = None;

        while !input.is_empty() {
            // `parse_any` accepts the keyword `crate`.
            let key = input.call(Ident::parse_any)?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "name" => name = Some(input.parse()?),
                "description" => description = Some(input.parse()?),
                "crate" => anchor = Some(input.parse()?),
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown attribute `{key}`, expected `name`, `description` or `crate`"
                        ),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let name = name.ok_or_else(|| input.error("missing required attribute `name`"))?;
        let description =
            description.ok_or_else(|| input.error("missing required attribute `description`"))?;
        Ok(Self {
            name,
            description,
            anchor,
        })
    }
}

/// Reports whether the item already derives `Clone`, so the [`tool`] attribute
/// never emits a second derive.
fn derives_clone(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        let mut found = false;
        let parsed = attribute.path().is_ident("derive")
            && attribute
                .parse_nested_meta(|meta| {
                    if meta.path.is_ident("Clone") {
                        found = true;
                    }
                    Ok(())
                })
                .is_ok();
        parsed && found
    })
}

/// Attaches a tool's static metadata to a struct.
///
/// The argument accepts `name = "..."`, `description = "..."` and an optional
/// `crate = <path>`, separated by commas and in any order. On expansion the
/// attribute derives `Clone` (unless the struct already does) and implements
/// `<crate>::ToolMetadata` with the given `name` and `description`; `crate`
/// defaults to `norte_core`.
///
/// Because `FnExecutor` and `ToolMetadata` are both present, the blanket
/// declarators apply and the struct registers without naming its argument and
/// result types: `ToolRegistry::register_declarator` for a plain result, and
/// `ToolRegistry::register_tool` for a rich `ToolOutput`.
///
/// The `norte` facade re-exports this attribute as `norte::tool`, with `crate`
/// defaulting to the facade.
///
/// # Example
///
/// ```ignore
/// #[norte_core::tool(name = "echo", description = "Echo the provided text")]
/// struct Echo;
///
/// #[async_trait::async_trait]
/// impl FnExecutor<EchoArgs, EchoResult> for Echo {
///     async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
///         Ok(EchoResult { echoed: args.text })
///     }
/// }
///
/// tools.register_declarator(&Echo)?;
/// ```
#[proc_macro_attribute]
pub fn tool(attribute: TokenStream, item: TokenStream) -> TokenStream {
    expand(attribute, item, syn::parse_quote!(::norte_core))
}

/// The same attribute, with `crate` defaulting to `norte`. The `norte` facade
/// re-exports it as `norte::tool`.
#[proc_macro_attribute]
pub fn tool_via_facade(attribute: TokenStream, item: TokenStream) -> TokenStream {
    expand(attribute, item, syn::parse_quote!(::norte))
}

/// Parses the metadata, derives `Clone` unless the item already does, and
/// implements `<anchor>::ToolMetadata` for it.
fn expand(attribute: TokenStream, item: TokenStream, default_anchor: Path) -> TokenStream {
    let metadata = parse_macro_input!(attribute as ToolMetadataArgs);
    let mut item = parse_macro_input!(item as ItemStruct);

    if !derives_clone(&item.attrs) {
        item.attrs.push(syn::parse_quote!(#[derive(Clone)]));
    }

    let (impl_generics, type_generics, where_clause) = item.generics.split_for_impl();
    let name = &item.ident;
    let tool_name = &metadata.name;
    let description = &metadata.description;
    let anchor = metadata.anchor.unwrap_or(default_anchor);

    let expanded = quote! {
        #item

        impl #impl_generics #anchor::ToolMetadata for #name #type_generics #where_clause {
            const NAME: &'static str = #tool_name;
            const DESCRIPTION: &'static str = #description;
        }
    };

    TokenStream::from(expanded)
}
