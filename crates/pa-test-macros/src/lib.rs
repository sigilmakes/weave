use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

#[proc_macro_attribute]
pub fn pa_test(args: TokenStream, input: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return syn::Error::new(
            proc_macro::Span::call_site().into(),
            "pa_test takes no arguments",
        )
        .to_compile_error()
        .into();
    }
    let mut function = parse_macro_input!(input as ItemFn);
    let name = &function.sig.ident;
    let guard = syn::parse_quote! {
        let _pa_test_timing = ::pa_test_scratch::TestTiming::start(
            concat!(module_path!(), "::", stringify!(#name))
        );
    };
    let first_statement = function
        .block
        .stmts
        .iter()
        .position(|statement| !matches!(statement, syn::Stmt::Item(_)))
        .unwrap_or(function.block.stmts.len());
    function.block.stmts.insert(first_statement, guard);
    quote!(#function).into()
}
