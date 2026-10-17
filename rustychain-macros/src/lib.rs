#![forbid(unsafe_code)]
#![doc = "Attribute macros for `RustyChain` tool declarations."]

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Ident, ItemStruct, LitStr, Token, parse_macro_input};

/// The `name = "..."` and `description = "..."` arguments of the [`tool`]
/// attribute, in any order.
struct ToolMetadataArgs {
    name: LitStr,
    description: LitStr,
}

impl Parse for ToolMetadataArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name: Option<LitStr> = None;
        let mut description: Option<LitStr> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "name" => name = Some(input.parse()?),
                "description" => description = Some(input.parse()?),
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown attribute `{key}`, expected `name` or `description`"),
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
        Ok(Self { name, description })
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
/// The argument accepts `name = "..."` and `description = "..."`, separated by
/// commas and in any order. On expansion the attribute derives `Clone` (unless
/// the struct already does) and implements `rustychain_core::ToolMetadata` with
/// the given `name` and `description`.
///
/// Because `FnExecutor` and `ToolMetadata` are both present, the blanket
/// declarators apply and the struct registers without naming its argument and
/// result types: `ToolRegistry::register_declarator` for a plain result, and
/// `ToolRegistry::register_tool` for a rich `ToolOutput`.
///
/// # Example
///
/// ```ignore
/// #[rustychain_core::tool(name = "echo", description = "Echo the provided text")]
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
    let metadata = parse_macro_input!(attribute as ToolMetadataArgs);
    let mut item = parse_macro_input!(item as ItemStruct);

    if !derives_clone(&item.attrs) {
        item.attrs.push(syn::parse_quote!(#[derive(Clone)]));
    }

    let (impl_generics, type_generics, where_clause) = item.generics.split_for_impl();
    let name = &item.ident;
    let tool_name = &metadata.name;
    let description = &metadata.description;

    let expanded = quote! {
        #item

        impl #impl_generics ::rustychain_core::ToolMetadata for #name #type_generics #where_clause {
            const NAME: &'static str = #tool_name;
            const DESCRIPTION: &'static str = #description;
        }
    };

    TokenStream::from(expanded)
}
