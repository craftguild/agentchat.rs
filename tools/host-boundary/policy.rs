//! Reject host mechanisms in shared policy using parsed Rust paths and decisions.
#[path = "labels.rs"]
mod labels;
#[path = "modules.rs"]
mod modules;
use proc_macro2::{TokenStream, TokenTree};
use std::collections::{HashMap, HashSet};
use syn::{
    Expr, Item, Lit, Pat, UseTree,
    visit::{self, Visit},
};

/// Check parsed sources together so re-exported aliases cannot cross the boundary.
pub fn check_files(sources: &[(String, String, bool)]) -> Result<(), String> {
    let files = sources
        .iter()
        .map(|(name, source, identity)| {
            syn::parse_file(source)
                .map(|file| (name, file, identity))
                .map_err(|error| format!("{name}: invalid Rust source: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let index = labels::Index::new(files.iter().map(|(name, file, _)| (name.as_str(), file)));
    let mut global = Policy::new(&index, 0);
    let scanned = sources
        .iter()
        .map(|(name, _, _)| modules::normalized(std::path::Path::new(name)))
        .collect();
    for (name, file, _) in &files {
        modules::check(name, file, &scanned)?;
    }
    loop {
        let before = (global.host_types.len(), global.variants.len());
        for (_, file, _) in &files {
            global.collect_items(&file.items);
        }
        if before == (global.host_types.len(), global.variants.len()) {
            break;
        }
    }
    for (name, file, identity) in &files {
        let mut policy = Policy {
            host_types: global.host_types.clone(),
            variants: global.variants.clone(),
            index: &index,
            scope: index.file(name),
            bindings: HashMap::new(),
            failed: false,
        };
        for item in &file.items {
            if **identity && identity_encoder(item) {
                continue;
            }
            policy.visit_item(item);
        }
        if policy.failed {
            return Err(format!(
                "{name}: concrete host dispatch is forbidden in shared policy."
            ));
        }
    }
    Ok(())
}

/// Test one source through exactly the production parser and visitor.
#[cfg(test)]
pub fn check(source: &str, identity: bool) -> Result<(), String> {
    check_files(&[("<source>".into(), source.into(), identity)])
}

struct Policy<'i, 'a> {
    host_types: HashSet<String>,
    variants: HashSet<String>,
    index: &'i labels::Index<'a>,
    scope: usize,
    bindings: HashMap<String, bool>,
    failed: bool,
}
impl<'i, 'a> Policy<'i, 'a> {
    fn new(index: &'i labels::Index<'a>, scope: usize) -> Self {
        Self {
            index,
            scope,
            host_types: HashSet::new(),
            variants: HashSet::new(),
            bindings: HashMap::new(),
            failed: false,
        }
    }
    fn scoped<T>(&mut self, node: &T, action: impl FnOnce(&mut Self)) {
        let previous = self.scope;
        let bindings = self.bindings.clone();
        self.scope = self.index.scope(node).unwrap_or(previous);
        action(self);
        self.scope = previous;
        self.bindings = bindings;
    }
    fn host_type(&self, name: &str) -> bool {
        name == "HostKind" || self.host_types.contains(name)
    }
    fn path(&self, path: &syn::Path) -> bool {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        names.iter().any(|name| self.variants.contains(name))
            || names
                .windows(2)
                .any(|p| self.host_type(&p[0]) && matches!(p[1].as_str(), "Codex" | "Claude"))
    }
    fn collect_use(&mut self, tree: &UseTree, prefix: Vec<String>) {
        match tree {
            UseTree::Path(p) => {
                let mut prefix = prefix;
                prefix.push(p.ident.to_string());
                self.collect_use(&p.tree, prefix);
            }
            UseTree::Name(n) => self.import(&prefix, &n.ident.to_string(), &n.ident.to_string()),
            UseTree::Rename(n) => self.import(&prefix, &n.ident.to_string(), &n.rename.to_string()),
            UseTree::Group(g) => {
                for item in &g.items {
                    self.collect_use(item, prefix.clone());
                }
            }
            UseTree::Glob(_) if prefix.last().is_some_and(|s| self.host_type(s)) => {
                self.variants.extend(["Codex".into(), "Claude".into()]);
            }
            UseTree::Glob(_) => {}
        }
    }
    fn import(&mut self, prefix: &[String], name: &str, alias: &str) {
        if self.host_type(name)
            || (name == "self" && prefix.last().is_some_and(|p| self.host_type(p)))
        {
            self.host_types.insert(alias.into());
        }
        if self.variants.contains(name)
            || (prefix.last().is_some_and(|s| self.host_type(s))
                && matches!(name, "Codex" | "Claude"))
        {
            self.variants.insert(alias.into());
        }
    }
    fn collect_items(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Use(item) => self.collect_use(&item.tree, vec![]),
                Item::Mod(module) => {
                    if let Some((_, items)) = &module.content {
                        self.collect_items(items);
                    }
                }
                Item::Type(item) => {
                    if let syn::Type::Path(t) = &*item.ty
                        && t.path
                            .segments
                            .last()
                            .is_some_and(|s| self.host_type(&s.ident.to_string()))
                    {
                        self.host_types.insert(item.ident.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    fn label_expr(&self, expr: &Expr) -> bool {
        self.index.expression(expr, self.scope, &self.bindings)
    }
    fn tokens(&self, stream: TokenStream) -> bool {
        let mut names = Vec::new();
        for token in stream {
            match token {
                TokenTree::Group(g) if self.tokens(g.stream()) => return true,
                TokenTree::Ident(i) => names.push(i.to_string()),
                _ => {}
            }
        }
        names.iter().any(|n| self.variants.contains(n))
            || names
                .windows(2)
                .any(|p| self.host_type(&p[0]) && matches!(p[1].as_str(), "Codex" | "Claude"))
    }
}
impl<'ast> Visit<'ast> for Policy<'_, '_> {
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.failed |= self.label_expr(&node.cond);
        visit::visit_expr_if(self, node);
    }
    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.failed |= self.label_expr(&node.cond);
        visit::visit_expr_while(self, node);
    }
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let host = matches!(&*node.self_ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| self.host_type(&s.ident.to_string())));
        let previous = self.host_types.contains("Self");
        if host {
            self.host_types.insert("Self".into());
        }
        self.scoped(node, |this| visit::visit_item_impl(this, node));
        if !previous {
            self.host_types.remove("Self");
        }
    }
    fn visit_block(&mut self, node: &'ast syn::Block) {
        for _ in 0..node.stmts.len().max(1) {
            for statement in &node.stmts {
                if let syn::Stmt::Item(item) = statement {
                    self.collect_items(std::slice::from_ref(item));
                }
            }
        }
        self.scoped(node, |this| visit::visit_block(this, node));
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.collect_use(&item.tree, vec![]);
        // A concrete variant import cannot be hidden behind a later alias.
        let mut probe = Self::new(self.index, self.scope);
        probe.host_types.clone_from(&self.host_types);
        probe.collect_use(&item.tree, vec![]);
        self.failed |= !probe.variants.is_empty();
    }
    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        self.failed |= self.path(&node.path);
        visit::visit_expr_path(self, node);
    }
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.scoped(node, |this| visit::visit_item_mod(this, node));
    }
    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.scoped(node, |this| visit::visit_item_trait(this, node));
    }
    fn visit_local(&mut self, node: &'ast syn::Local) {
        let value = node
            .init
            .as_ref()
            .is_some_and(|init| self.label_expr(&init.expr));
        visit::visit_local(self, node);
        if let Pat::Ident(p) = &node.pat {
            self.bindings.insert(p.ident.to_string(), value);
        }
    }
    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        self.failed |= self.label_expr(&node.left) || self.label_expr(&node.right);
        visit::visit_expr_binary(self, node);
    }
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if matches!(
            node.method.to_string().as_str(),
            "eq" | "ne"
                | "eq_ignore_ascii_case"
                | "contains"
                | "starts_with"
                | "ends_with"
                | "cmp"
                | "partial_cmp"
        ) {
            self.failed |=
                self.label_expr(&node.receiver) || node.args.iter().any(|arg| self.label_expr(arg));
        }
        visit::visit_expr_method_call(self, node);
    }
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // Shared policy cannot hide production in unchecked generated/test files.
        self.failed |= node.path.is_ident("include");
        self.failed |= self.tokens(node.tokens.clone());
        // matches! expands to a match; parse its pattern instead of treating
        // string values in ordinary diagnostics/serialization as decisions.
        if node
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident == "matches")
        {
            if let Ok(args) = syn::parse2::<MatchArgs>(node.tokens.clone()) {
                self.visit_expr(&args.expr);
                self.visit_pat(&args.pattern);
                if let Some(guard) = &args.guard {
                    self.visit_expr(guard);
                }
            } else {
                self.failed = true;
            }
        }
    }
    fn visit_pat(&mut self, node: &'ast syn::Pat) {
        // syn cannot resolve a bare constant versus a new binding. Use only
        // collected constants here, keeping diagnostic let bindings harmless.
        self.failed |= match node {
            Pat::Lit(literal) => self.label_expr(&Expr::Lit(literal.clone())),
            Pat::Ident(p) => {
                self.index
                    .pattern(&syn::Path::from(p.ident.clone()), None, self.scope)
            }
            Pat::Path(p) => self.index.pattern(&p.path, p.qself.as_ref(), self.scope),
            _ => false,
        };
        visit::visit_pat(self, node);
    }
}
struct MatchArgs {
    expr: Expr,
    pattern: Pat,
    guard: Option<Expr>,
}
impl syn::parse::Parse for MatchArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let expr = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let pattern = Pat::parse_multi_with_leading_vert(input)?;
        let guard = if input.peek(syn::Token![if]) {
            input.parse::<syn::Token![if]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        if input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;
        }
        Ok(Self {
            expr,
            pattern,
            guard,
        })
    }
}
fn identity_encoder(item: &Item) -> bool {
    let Item::Impl(item) = item else { return false };
    let syn::Type::Path(ty) = &*item.self_ty else {
        return false;
    };
    if !ty.path.is_ident("HostKind") || item.items.len() != 1 || item.trait_.is_some() {
        return false;
    }
    let syn::ImplItem::Fn(method) = &item.items[0] else {
        return false;
    };
    if method.sig.ident != "as_str" || method.block.stmts.len() != 1 {
        return false;
    }
    let syn::Stmt::Expr(Expr::Match(expr), _) = &method.block.stmts[0] else {
        return false;
    };
    if !matches!(&*expr.expr, Expr::Path(p) if p.path.is_ident("self")) || expr.arms.len() != 2 {
        return false;
    }
    expr.arms.iter().zip([("Codex","codex"),("Claude","claude")]).all(|(a,(variant,label))| {
        a.guard.is_none() && matches!(&a.pat, Pat::Path(p) if p.path.segments.len()==2 && p.path.segments[0].ident=="Self" && p.path.segments[1].ident==variant)
            && matches!(&*a.body, Expr::Lit(l) if matches!(&l.lit,Lit::Str(s) if s.value()==label))
    })
}
