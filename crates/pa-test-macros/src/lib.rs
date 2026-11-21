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
    let function = instrument(parse_macro_input!(input as ItemFn));
    quote!(#function).into()
}

fn instrument(mut function: ItemFn) -> ItemFn {
    let name = &function.sig.ident;
    let guard = syn::parse_quote! {
        let _pa_test_timing = if ::pa_test_scratch::TestTiming::is_enabled() {
            ::pa_test_scratch::TestTiming::start(
                concat!(module_path!(), "::", stringify!(#name))
            )
        } else {
            ::pa_test_scratch::TestTiming::default()
        };
    };
    let first_statement = function
        .block
        .stmts
        .iter()
        .position(|statement| !matches!(statement, syn::Stmt::Item(_)))
        .unwrap_or(function.block.stmts.len());
    function.block.stmts.insert(first_statement, guard);
    function
}

#[cfg(test)]
mod tests {
    use super::instrument;
    use syn::{Expr, Item, Pat, Stmt};

    #[test]
    fn expansion_keeps_leading_items_before_the_first_local_guard() {
        let original = syn::parse_quote! {
            #[test]
            fn sample() {
                use std::time::Instant;
                struct Fixture;
                let fixture = Fixture;
                drop(fixture);
            }
        };
        let expanded = instrument(original);
        assert_eq!(expanded.attrs.len(), 1);
        assert!(expanded.attrs[0].path().is_ident("test"));
        assert!(matches!(expanded.block.stmts[0], Stmt::Item(Item::Use(_))));
        assert!(matches!(
            expanded.block.stmts[1],
            Stmt::Item(Item::Struct(_))
        ));
        let Stmt::Local(guard) = &expanded.block.stmts[2] else {
            panic!("the first executable statement must create the guard");
        };
        let Pat::Ident(binding) = &guard.pat else {
            panic!("the guard must be a named local");
        };
        assert_eq!(binding.ident, "_pa_test_timing");
        let Stmt::Local(fixture) = &expanded.block.stmts[3] else {
            panic!("fixture locals must follow the guard");
        };
        let Pat::Ident(binding) = &fixture.pat else {
            panic!("the original fixture binding must remain intact");
        };
        assert_eq!(binding.ident, "fixture");
    }

    #[test]
    fn disabled_expansion_branch_has_no_record_start_call() {
        let expanded = instrument(syn::parse_quote! { fn sample() {} });
        let Stmt::Local(guard) = &expanded.block.stmts[0] else {
            panic!("the expansion must create a guard");
        };
        let Expr::If(branch) = &*guard.init.as_ref().expect("guard initializer").expr else {
            panic!("recording must be conditional at runtime");
        };
        let Expr::Call(condition) = &*branch.cond else {
            panic!("the condition must read the captured enabled state");
        };
        let Expr::Path(condition_path) = &*condition.func else {
            panic!("the condition must call the timing API");
        };
        assert_eq!(
            condition_path.path.segments.last().unwrap().ident,
            "is_enabled"
        );
        let Expr::Block(disabled) = &*branch.else_branch.as_ref().expect("disabled branch").1
        else {
            panic!("disabled recording must have its own block");
        };
        assert_eq!(disabled.block.stmts.len(), 1);
        let Stmt::Expr(Expr::Call(call), None) = &disabled.block.stmts[0] else {
            panic!("the disabled branch must only construct an inert guard");
        };
        let Expr::Path(path) = &*call.func else {
            panic!("the disabled branch must call Default");
        };
        assert_eq!(path.path.segments.last().unwrap().ident, "default");
        assert!(call.args.is_empty());
    }
}
