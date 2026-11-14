//! Resolve label constants by lexical/module identity, without evaluating Rust.
#[path = "scopes.rs"]
mod scopes;
use std::collections::{HashMap, HashSet};
use syn::{Expr, Lit, Path, QSelf};

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    module: usize,
    root: usize,
    outer_module: Option<usize>,
    names: HashMap<String, usize>,
    types: HashMap<String, usize>,
    aliases: HashMap<String, Vec<usize>>,
    globs: Vec<Path>,
    fallback: Vec<usize>,
}
enum Symbol<'a> {
    Namespace(usize),
    Constant(&'a Expr, usize),
    // `type` aliases introduce no value binding; `use` can import both spaces.
    Alias(Path, usize, bool),
}

/// A bounded syntax index: declarations, imports and explicit associated paths.
/// Constant cycles and alias cycles terminate without becoming host labels.
#[derive(Default)]
pub struct Index<'a> {
    scopes: Vec<Scope>,
    symbols: Vec<Symbol<'a>>,
    nodes: HashMap<usize, usize>,
    files: HashMap<String, usize>,
    implementations: HashMap<(usize, usize), usize>,
}
impl<'a> Index<'a> {
    /// Index the complete scanned set before resolving any forward references.
    pub fn new(files: impl IntoIterator<Item = (&'a str, &'a syn::File)>) -> Self {
        let mut index = Self::default();
        scopes::collect(&mut index, files.into_iter().collect());
        index
    }
    /// Return the module assigned to a scanned source file.
    pub fn file(&self, name: &str) -> usize {
        self.files[name]
    }
    /// Return the lexical scope of an original AST node (never a macro clone).
    pub fn scope<T>(&self, node: &T) -> Option<usize> {
        self.nodes.get(&(node as *const T as usize)).copied()
    }
    /// Test an expression's syntax and referenced constants, preserving local
    /// value shadowing. Constants never capture a function's dynamic bindings.
    pub fn expression(&self, expr: &Expr, scope: usize, bindings: &HashMap<String, bool>) -> bool {
        self.expr(expr, scope, Some(bindings), &mut HashSet::new(), None)
    }
    /// Only constants, not let bindings, can classify identifier patterns.
    pub fn pattern(&self, path: &Path, qself: Option<&QSelf>, scope: usize) -> bool {
        self.reference(scope, path, qself, &mut HashSet::new(), None)
    }
    fn reference(
        &self,
        scope: usize,
        path: &Path,
        qself: Option<&QSelf>,
        seen: &mut HashSet<usize>,
        self_namespace: Option<usize>,
    ) -> bool {
        // A default's Self::OTHER must resolve in the selected implementation,
        // including through a block tail, while ordinary names retain the
        // default's declaration scope. Do not evaluate all defaults globally.
        let namespace = if let Some(qself) = qself {
            let ty = self.type_namespace(scope, &qself.ty);
            if qself.position == 0 {
                ty
            } else {
                let mut prefix = path.clone();
                prefix.segments = path.segments.iter().take(qself.position).cloned().collect();
                self.resolve(scope, &prefix, None, &mut HashSet::new(), true)
                    .and_then(|id| self.namespace(id))
                    .map(|brand| {
                        ty.and_then(|ty| self.implementations.get(&(ty, brand)).copied())
                            .unwrap_or(brand)
                    })
            }
        } else if path.segments.len() > 1 {
            let mut prefix = path.clone();
            prefix.segments.pop();
            prefix.segments.pop_punct();
            if prefix.is_ident("Self") && self_namespace.is_some() {
                self_namespace
            } else {
                self.resolve(scope, &prefix, None, &mut HashSet::new(), true)
                    .and_then(|id| self.namespace(id))
            }
        } else {
            None
        };
        let symbol = if let Some(namespace) = namespace {
            path.segments.last().and_then(|s| {
                self.member(namespace, &s.ident.to_string(), &mut HashSet::new(), false)
            })
        } else {
            self.resolve(scope, path, qself, &mut HashSet::new(), false)
        };
        symbol.is_some_and(|id| self.constant(id, seen, namespace))
    }
    fn constant(
        &self,
        symbol: usize,
        seen: &mut HashSet<usize>,
        self_namespace: Option<usize>,
    ) -> bool {
        if !seen.insert(symbol) {
            return false;
        }
        let result = match self.symbols[symbol] {
            Symbol::Constant(expr, scope) => self.expr(expr, scope, None, seen, self_namespace),
            _ => false,
        };
        seen.remove(&symbol);
        result
    }
    fn expr(
        &self,
        expr: &Expr,
        scope: usize,
        bindings: Option<&HashMap<String, bool>>,
        seen: &mut HashSet<usize>,
        self_namespace: Option<usize>,
    ) -> bool {
        let eval =
            |e, seen: &mut HashSet<usize>| self.expr(e, scope, bindings, seen, self_namespace);
        match expr {
            Expr::Lit(l) => match &l.lit {
                Lit::Str(s) => {
                    matches!(s.value().to_ascii_lowercase().as_str(), "codex" | "claude")
                }
                Lit::ByteStr(s) => matches!(s.value().as_slice(), b"codex" | b"claude"),
                _ => false,
            },
            Expr::Path(p) => {
                if p.qself.is_none()
                    && let Some(name) = p.path.get_ident()
                    && let Some(value) = bindings.and_then(|b| b.get(&name.to_string()))
                {
                    return *value;
                }
                self.reference(scope, &p.path, p.qself.as_ref(), seen, self_namespace)
            }
            Expr::Paren(p) => eval(&p.expr, seen),
            Expr::Group(p) => eval(&p.expr, seen),
            Expr::Reference(r) => eval(&r.expr, seen),
            Expr::Block(b) => self.block(&b.block, scope, bindings, seen, self_namespace),
            Expr::Const(b) => self.block(&b.block, scope, bindings, seen, self_namespace),
            Expr::Array(a) => a.elems.iter().any(|e| eval(e, seen)),
            Expr::Index(i) => eval(&i.expr, seen),
            Expr::MethodCall(c) => eval(&c.receiver, seen) || c.args.iter().any(|e| eval(e, seen)),
            Expr::Call(c) => c.args.iter().any(|e| eval(e, seen)),
            _ => false,
        }
    }
    fn block(
        &self,
        block: &syn::Block,
        scope: usize,
        bindings: Option<&HashMap<String, bool>>,
        seen: &mut HashSet<usize>,
        self_namespace: Option<usize>,
    ) -> bool {
        matches!(block.stmts.last(), Some(syn::Stmt::Expr(expr, None))
            if self.expr(expr, self.scope(block).unwrap_or(scope), bindings, seen, self_namespace))
    }
    fn namespace(&self, symbol: usize) -> Option<usize> {
        match self.symbols[symbol] {
            Symbol::Namespace(scope) => Some(scope),
            _ => None,
        }
    }
    fn type_namespace(&self, scope: usize, ty: &syn::Type) -> Option<usize> {
        let syn::Type::Path(ty) = ty else { return None };
        self.resolve(
            scope,
            &ty.path,
            ty.qself.as_ref(),
            &mut HashSet::new(),
            true,
        )
        .and_then(|id| self.namespace(id))
    }
    fn resolve(
        &self,
        scope: usize,
        path: &Path,
        qself: Option<&QSelf>,
        seen: &mut HashSet<usize>,
        types: bool,
    ) -> Option<usize> {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if let Some(qself) = qself {
            let ty = self.type_namespace(scope, &qself.ty)?;
            let namespace = if qself.position == 0 {
                ty
            } else {
                let mut trait_path = path.clone();
                trait_path.segments = path.segments.iter().take(qself.position).cloned().collect();
                let brand = self.resolve(scope, &trait_path, None, seen, true)?;
                let brand = self.namespace(brand)?;
                *self.implementations.get(&(ty, brand)).unwrap_or(&brand)
            };
            return self.members(namespace, &names[qself.position..], seen, types);
        }
        let (first, rest) = names.split_first()?;
        let module = self.scopes[scope].module;
        match first.as_str() {
            "crate" => self.members(self.scopes[scope].root, rest, seen, types),
            "self" => self.members(module, rest, seen, types),
            "super" => {
                let mut module = self.scopes[module].outer_module?;
                let mut rest = rest;
                while rest.first().is_some_and(|s| s == "super") {
                    module = self.scopes[module].outer_module?;
                    rest = &rest[1..];
                }
                self.members(module, rest, seen, types)
            }
            _ => {
                let mut cursor = Some(if path.leading_colon.is_some() {
                    self.scopes[scope].root
                } else {
                    scope
                });
                while let Some(current) = cursor {
                    if let Some(id) = self.member(current, first, seen, types || !rest.is_empty()) {
                        return if rest.is_empty() {
                            Some(id)
                        } else {
                            self.members(self.namespace(id)?, rest, seen, types)
                        };
                    }
                    cursor = self.scopes[current].parent;
                }
                None
            }
        }
    }
    fn members(
        &self,
        scope: usize,
        names: &[String],
        seen: &mut HashSet<usize>,
        types: bool,
    ) -> Option<usize> {
        let (first, rest) = names.split_first()?;
        let id = self.member(scope, first, seen, types || !rest.is_empty())?;
        if rest.is_empty() {
            Some(id)
        } else {
            self.members(self.namespace(id)?, rest, seen, types)
        }
    }
    fn member(
        &self,
        scope: usize,
        name: &str,
        seen: &mut HashSet<usize>,
        types: bool,
    ) -> Option<usize> {
        // Rust permits a named-field struct and a constant with the same name.
        // Prefix/type lookup and final value lookup must remain independent.
        let declarations = if types {
            &self.scopes[scope].types
        } else {
            &self.scopes[scope].names
        };
        if let Some(&id) = declarations.get(name) {
            return Some(id);
        }
        if let Some(aliases) = self.scopes[scope].aliases.get(name) {
            for &id in aliases {
                if !seen.insert(id) {
                    continue;
                }
                let result = match &self.symbols[id] {
                    Symbol::Alias(path, origin, type_only) if types || !type_only => {
                        self.resolve(*origin, path, None, seen, types)
                    }
                    _ => None,
                };
                seen.remove(&id);
                if result.is_some() {
                    return result;
                }
            }
        }
        // A glob edge can revisit a module without an alias symbol. Guard it
        // separately so cyclic re-exports remain bounded as well.
        let edge = self.symbols.len() + scope;
        if !seen.insert(edge) {
            return None;
        }
        let result = self.scopes[scope]
            .globs
            .iter()
            .find_map(|path| {
                let id = self.resolve(scope, path, None, seen, true)?;
                self.member(self.namespace(id)?, name, seen, types)
            })
            .or_else(|| {
                self.scopes[scope]
                    .fallback
                    .iter()
                    .find_map(|s| self.member(*s, name, seen, types))
            });
        seen.remove(&edge);
        result
    }
}
