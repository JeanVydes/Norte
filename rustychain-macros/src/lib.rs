use proc_macro::TokenStream;
use quote::quote;
use syn::{
    DeriveInput, Ident, ItemStruct, LitStr, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

struct FunctionMeta {
    name: LitStr,
    description: LitStr,
    args: Type,
    result: Type,
}

impl Parse for FunctionMeta {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut description = None;
        let mut args = None;
        let mut result = None;

        // Parsear cada par clave = valor
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "name" => {
                    name = Some(input.parse::<LitStr>()?);
                }
                "description" => {
                    description = Some(input.parse::<LitStr>()?);
                }
                "args" => {
                    args = Some(input.parse::<Type>()?);
                }
                "result" => {
                    result = Some(input.parse::<Type>()?);
                }
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("Unknown attribute: {}", key),
                    ));
                }
            }

            // Consumir coma opcional
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(FunctionMeta {
            name: name.ok_or_else(|| input.error("Missing 'name' attribute"))?,
            description: description
                .ok_or_else(|| input.error("Missing 'description' attribute"))?,
            args: args.ok_or_else(|| input.error("Missing 'args' attribute"))?,
            result: result.ok_or_else(|| input.error("Missing 'result' attribute"))?,
        })
    }
}

#[proc_macro_attribute]
pub fn declare_function(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let meta = parse_macro_input!(attr as FunctionMeta);

    let struct_name = &input.ident;
    let struct_vis = &input.vis;
    let struct_attrs = &input.attrs;
    let struct_fields = &input.fields;

    let fn_name = &meta.name;
    let fn_description = &meta.description;
    let args_type = &meta.args;
    let result_type = &meta.result;

    let expanded = quote! {
        #(#struct_attrs)*
        #[derive(Clone)]
        #struct_vis struct #struct_name #struct_fields

        #[async_trait::async_trait]
        impl ::rustychain::FnExecutor<#args_type, #result_type> for #struct_name {
            async fn call(&self, args: #args_type) -> ::rustychain::Result<#result_type> {
                self.execute(args).await
            }
        }

        impl ::rustychain::FnDeclarator<#args_type, #result_type> for #struct_name {
            fn declare(&self) -> ::rustychain::FunctionDeclaration<#args_type, #result_type> {
                ::rustychain::FunctionDeclaration {
                    name: #fn_name,
                    description: #fn_description,
                    parameters: ::schemars::schema_for!(#args_type),
                    executor: ::std::sync::Arc::new(self.clone()),
                }
            }
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_derive(ToolArgs)]
pub fn derive_tool_args(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let expanded = quote! {
        #[automatically_derived]
        impl #impl_generics ::rustychain::ToolArgs for #struct_name #ty_generics #where_clause {}
    };

    TokenStream::from(expanded)
}

/// Metadata for runnable step declaration
struct RunnableMeta {
    input: Type,
    output: Type,
    method: Option<Ident>,
}

impl Parse for RunnableMeta {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut input_type: Option<Type> = None;
        let mut output_type: Option<Type> = None;
        let mut method: Option<Ident> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "input" => {
                    input_type = Some(input.parse()?);
                }
                "output" => {
                    output_type = Some(input.parse()?);
                }
                "method" => {
                    let method_lit: syn::LitStr = input.parse()?;
                    method = Some(Ident::new(&method_lit.value(), method_lit.span()));
                }
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("Unknown attribute: {}", key),
                    ));
                }
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(RunnableMeta {
            input: input_type.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "Missing required attribute: input",
                )
            })?,
            output: output_type.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "Missing required attribute: output",
                )
            })?,
            method,
        })
    }
}

#[proc_macro_attribute]
pub fn runnable(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let meta = parse_macro_input!(attr as RunnableMeta);

    let struct_name = &input.ident;
    let struct_vis = &input.vis;
    let struct_attrs = &input.attrs;
    let struct_generics = &input.generics;
    let struct_fields = &input.fields;

    let input_type = &meta.input;
    let output_type = &meta.output;
    let method_name = meta
        .method
        .unwrap_or_else(|| Ident::new("execute", proc_macro2::Span::call_site()));

    let expanded = quote! {
        #(#struct_attrs)*
        #[derive(Clone)]
        #struct_vis struct #struct_name #struct_generics #struct_fields

        #[async_trait::async_trait]
        #[async_trait::async_trait]
        impl ::rustychain::chain::step::Runnable<#input_type, #output_type> for #struct_name {
            async fn call(&self, input: ::std::sync::Arc<#input_type>) -> ::rustychain::Result<::std::sync::Arc<#output_type>> {
                // Call the specified method to get the output. Should we pass Arc or deref?
                // By now, we deref the Arc to pass the inner value to simplify usage.
                // Trade-off between usability and performance.
                let result = self.#method_name((*input).clone()).await?;
                // Return the result wrapped in Arc
                Ok(::std::sync::Arc::new(result))
            }
        }
    };

    TokenStream::from(expanded)
}
