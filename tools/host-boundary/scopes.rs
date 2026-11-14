//! Collect distinct module, block and associated-constant namespaces.
use super::{Index, Scope, Symbol};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use syn::{
    ItemImpl, UseTree,
    visit::{self, Visit},
};

pub(super) fn collect<'a>(index: &mut Index<'a>, files: Vec<(&'a str, &'a syn::File)>) {
    let mut roots = HashMap::new();
    let mut paths = HashMap::new();
    for (name, _) in &files {
        let path = Path::new(name);
        let parts: Vec<_> = path.iter().collect();
        let src = parts.iter().rposition(|p| *p == "src");
        let base: PathBuf = match src {
            Some(i) => parts[..=i].iter().collect(),
            None => path.parent().unwrap_or(Path::new("")).into(),
        };
        let root = *roots.entry(base.clone()).or_insert_with(|| index.root());
        let mut scope = root;
        if files.len() > 1 {
            let relative = path.strip_prefix(&base).unwrap_or(path);
            for component in relative.parent().unwrap_or(Path::new("")).iter() {
                scope = index.named(scope, &component.to_string_lossy(), true);
            }
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            if !matches!(stem.as_ref(), "lib" | "main" | "mod" | "<source>") {
                scope = index.named(scope, &stem, true);
            }
        }
        index.files.insert((*name).into(), scope);
        paths.insert(super::super::modules::normalized(path), scope);
    }
    let mut pending = Vec::new();
    for (name, file) in files {
        let scope = index.file(name);
        let mut collector = Collector {
            index,
            scope,
            directory: Path::new(name).parent().unwrap_or(Path::new("")).into(),
            paths: &paths,
            pending: &mut pending,
        };
        collector.visit_file(file);
    }
    // Types/imports may occur after their impls or in later files. Link only
    // after collecting all declarations; trait overrides take precedence over
    // defaults and qualified trait paths retain their particular impl.
    for (origin, scope, item) in pending {
        let ty = index.ensure_type(origin, &item.self_ty);
        let brand = item
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| {
                index.resolve(origin, path, None, &mut Default::default(), true)
            })
            .and_then(|id| index.namespace(id));
        if let Some(brand) = brand {
            index.scopes[scope].fallback.push(brand);
            if let Some(ty) = ty {
                index.implementations.insert((ty, brand), scope);
            }
        }
        let target = if brand.is_some() {
            scope
        } else {
            ty.unwrap_or(scope)
        };
        index.symbol(scope, "Self".into(), Symbol::Namespace(target));
        if let Some(ty) = ty {
            if brand.is_some() {
                index.scopes[ty].fallback.push(scope);
                continue;
            }
            let names = index.scopes[scope].names.clone();
            for (name, id) in names {
                if name != "Self" {
                    index.scopes[ty].names.entry(name).or_insert(id);
                }
            }
        }
    }
}
impl<'a> Index<'a> {
    // An impl may target an external/prelude type whose declaration is outside
    // the scanned set. Retain its explicit path identity instead of dropping
    // the impl's constants; imports/type aliases still point to that identity.
    fn ensure_type(&mut self, scope: usize, ty: &syn::Type) -> Option<usize> {
        let syn::Type::Path(ty) = ty else { return None };
        self.ensure_path(scope, &ty.path, &mut Default::default())
    }
    fn ensure_path(
        &mut self,
        scope: usize,
        path: &syn::Path,
        seen: &mut std::collections::HashSet<usize>,
    ) -> Option<usize> {
        if let Some(id) = self.resolve(scope, path, None, &mut Default::default(), true)
            && let Some(namespace) = self.namespace(id)
        {
            return Some(namespace);
        }
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let (first, mut rest) = names.split_first()?;
        let mut current = match first.as_str() {
            "crate" => self.scopes[scope].root,
            "self" => self.scopes[scope].module,
            "super" => {
                let mut parent = self.scopes[self.scopes[scope].module].outer_module?;
                while rest.first().is_some_and(|s| s == "super") {
                    parent = self.scopes[parent].outer_module?;
                    rest = &rest[1..];
                }
                parent
            }
            _ => {
                let mut cursor = Some(scope);
                let mut symbol = None;
                while let Some(current) = cursor {
                    if let Some(&id) = self.scopes[current].types.get(first) {
                        symbol = Some(id);
                        break;
                    }
                    if let Some(aliases) = self.scopes[current].aliases.get(first) {
                        symbol = aliases.last().copied();
                        break;
                    }
                    if let Some(&id) = self.scopes[current].names.get(first) {
                        symbol = Some(id);
                        break;
                    }
                    cursor = self.scopes[current].parent;
                }
                match symbol {
                    Some(id) => match &self.symbols[id] {
                        Symbol::Namespace(namespace) => *namespace,
                        Symbol::Alias(path, origin, _) => {
                            if !seen.insert(id) {
                                return None;
                            }
                            let (path, origin) = (path.clone(), *origin);
                            self.ensure_path(origin, &path, seen)?
                        }
                        _ => return None,
                    },
                    None => self.named(self.scopes[scope].module, first, false),
                }
            }
        };
        for name in rest {
            current = self.named(current, name, false);
        }
        Some(current)
    }
    fn root(&mut self) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            module: id,
            root: id,
            ..Default::default()
        });
        id
    }
    fn symbol(&mut self, scope: usize, name: String, symbol: Symbol<'a>) {
        let id = self.symbols.len();
        self.symbols.push(symbol);
        match &self.symbols[id] {
            Symbol::Namespace(_) => {
                self.scopes[scope].types.insert(name, id);
            }
            Symbol::Constant(..) => {
                self.scopes[scope].names.insert(name, id);
            }
            Symbol::Alias(..) => {
                self.scopes[scope].aliases.entry(name).or_default().push(id);
            }
        }
    }
    fn nested(&mut self, parent: usize, module: bool) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: if module { None } else { Some(parent) },
            module: if module {
                id
            } else {
                self.scopes[parent].module
            },
            root: self.scopes[parent].root,
            outer_module: if module {
                Some(self.scopes[parent].module)
            } else {
                None
            },
            ..Default::default()
        });
        id
    }
    fn named(&mut self, parent: usize, name: &str, module: bool) -> usize {
        if let Some(scope) = self.scopes[parent]
            .types
            .get(name)
            .and_then(|id| self.namespace(*id))
        {
            return scope;
        }
        let id = self.nested(parent, module);
        self.symbol(parent, name.into(), Symbol::Namespace(id));
        id
    }
}
struct Collector<'i, 'a> {
    index: &'i mut Index<'a>,
    scope: usize,
    directory: PathBuf,
    paths: &'i HashMap<PathBuf, usize>,
    pending: &'i mut Vec<(usize, usize, &'a ItemImpl)>,
}
impl<'a> Collector<'_, 'a> {
    fn enter<T>(&mut self, node: &T, scope: usize, visit: impl FnOnce(&mut Self)) {
        self.index.nodes.insert(node as *const T as usize, scope);
        let previous = std::mem::replace(&mut self.scope, scope);
        visit(self);
        self.scope = previous;
    }
    fn imports(&mut self, tree: &UseTree, mut prefix: Vec<syn::Ident>) {
        match tree {
            UseTree::Path(p) => {
                prefix.push(p.ident.clone());
                self.imports(&p.tree, prefix);
            }
            UseTree::Group(g) => {
                for tree in &g.items {
                    self.imports(tree, prefix.clone());
                }
            }
            UseTree::Glob(_) => {
                self.index.scopes[self.scope].globs.push(syn::Path {
                    leading_colon: None,
                    segments: prefix.into_iter().map(syn::PathSegment::from).collect(),
                });
            }
            UseTree::Name(n) => {
                let alias = if n.ident == "self" {
                    prefix.last().cloned().unwrap_or_else(|| n.ident.clone())
                } else {
                    n.ident.clone()
                };
                self.import(prefix, &n.ident, &alias);
            }
            UseTree::Rename(n) => self.import(prefix, &n.ident, &n.rename),
        }
    }
    fn import(&mut self, mut prefix: Vec<syn::Ident>, name: &syn::Ident, alias: &syn::Ident) {
        if name != "self" {
            prefix.push(name.clone());
        }
        let path = syn::Path {
            leading_colon: None,
            segments: prefix.into_iter().map(syn::PathSegment::from).collect(),
        };
        self.index.symbol(
            self.scope,
            alias.to_string(),
            Symbol::Alias(path, self.scope, false),
        );
    }
}
impl<'a> Visit<'a> for Collector<'_, 'a> {
    fn visit_item_mod(&mut self, node: &'a syn::ItemMod) {
        let scope = self.index.named(self.scope, &node.ident.to_string(), true);
        if node.content.is_some() {
            let previous = self.directory.clone();
            self.directory.push(node.ident.to_string());
            self.enter(node, scope, |this| visit::visit_item_mod(this, node));
            self.directory = previous;
        } else {
            for attr in &node.attrs {
                if let syn::Meta::NameValue(value) = &attr.meta
                    && value.path.is_ident("path")
                    && let syn::Expr::Lit(expr) = &value.value
                    && let syn::Lit::Str(path) = &expr.lit
                    && let Some(&target) = self.paths.get(&super::super::modules::normalized(
                        &self.directory.join(path.value()),
                    ))
                {
                    // An explicit file path chooses storage, not the logical
                    // module parent. super/crate references use the declaring
                    // module even when the file lives in another directory.
                    let root = self.index.scopes[self.scope].root;
                    let previous_root = self.index.scopes[target].root;
                    for scope in &mut self.index.scopes {
                        if scope.root == previous_root {
                            scope.root = root;
                        }
                    }
                    self.index.scopes[target].outer_module =
                        Some(self.index.scopes[self.scope].module);
                    self.index.symbol(
                        self.scope,
                        node.ident.to_string(),
                        Symbol::Namespace(target),
                    );
                }
            }
        }
    }
    fn visit_item_struct(&mut self, node: &'a syn::ItemStruct) {
        self.index.named(self.scope, &node.ident.to_string(), false);
        visit::visit_item_struct(self, node);
    }
    fn visit_item_enum(&mut self, node: &'a syn::ItemEnum) {
        self.index.named(self.scope, &node.ident.to_string(), false);
        visit::visit_item_enum(self, node);
    }
    fn visit_item_trait(&mut self, node: &'a syn::ItemTrait) {
        let scope = self.index.named(self.scope, &node.ident.to_string(), false);
        self.index
            .symbol(scope, "Self".into(), Symbol::Namespace(scope));
        self.enter(node, scope, |this| visit::visit_item_trait(this, node));
    }
    fn visit_item_impl(&mut self, node: &'a ItemImpl) {
        let scope = self.index.nested(self.scope, false);
        self.pending.push((self.scope, scope, node));
        self.enter(node, scope, |this| visit::visit_item_impl(this, node));
    }
    fn visit_item_const(&mut self, node: &'a syn::ItemConst) {
        self.index.symbol(
            self.scope,
            node.ident.to_string(),
            Symbol::Constant(&node.expr, self.scope),
        );
        visit::visit_item_const(self, node);
    }
    fn visit_impl_item_const(&mut self, node: &'a syn::ImplItemConst) {
        self.index.symbol(
            self.scope,
            node.ident.to_string(),
            Symbol::Constant(&node.expr, self.scope),
        );
        visit::visit_impl_item_const(self, node);
    }
    fn visit_trait_item_const(&mut self, node: &'a syn::TraitItemConst) {
        if let Some((_, expr)) = &node.default {
            self.index.symbol(
                self.scope,
                node.ident.to_string(),
                Symbol::Constant(expr, self.scope),
            );
        }
        visit::visit_trait_item_const(self, node);
    }
    fn visit_item_use(&mut self, node: &'a syn::ItemUse) {
        self.imports(&node.tree, Vec::new());
    }
    fn visit_item_type(&mut self, node: &'a syn::ItemType) {
        if let syn::Type::Path(ty) = &*node.ty {
            self.index.symbol(
                self.scope,
                node.ident.to_string(),
                Symbol::Alias(ty.path.clone(), self.scope, true),
            );
        }
        visit::visit_item_type(self, node);
    }
    fn visit_block(&mut self, node: &'a syn::Block) {
        let scope = self.index.nested(self.scope, false);
        self.enter(node, scope, |this| visit::visit_block(this, node));
    }
}
