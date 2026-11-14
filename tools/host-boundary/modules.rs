//! Explicit module paths cannot move production policy outside the parsed set.
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};
use syn::{
    Expr, Lit, Meta,
    visit::{self, Visit},
};

/// Check every explicit path, including conditional path attributes.
pub fn check(name: &str, file: &syn::File, scanned: &HashSet<PathBuf>) -> Result<(), String> {
    let mut checker = Modules {
        directory: Path::new(name)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .into(),
        scanned,
        invalid: false,
    };
    checker.visit_file(file);
    if checker.invalid {
        Err(format!(
            "{name}: module path escapes the parsed policy source set."
        ))
    } else {
        Ok(())
    }
}

/// Lexical normalization only: no host probe or filesystem-dependent exemption.
pub fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Preserve relative underflow: dropping it could make an
                // outside file appear to belong to the checked source set.
                if result.file_name() == Some(std::ffi::OsStr::new(".."))
                    || (!result.pop() && !path.is_absolute())
                {
                    result.push("..");
                }
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}
struct Modules<'a> {
    directory: PathBuf,
    scanned: &'a HashSet<PathBuf>,
    invalid: bool,
}
impl Modules<'_> {
    fn attribute(&mut self, meta: &Meta) {
        match meta {
            Meta::NameValue(value) if value.path.is_ident("path") => {
                if let Expr::Lit(literal) = &value.value
                    && let Lit::Str(path) = &literal.lit
                {
                    self.invalid |= !self
                        .scanned
                        .contains(&normalized(&self.directory.join(path.value())));
                } else {
                    self.invalid = true;
                }
            }
            Meta::List(list) if list.path.is_ident("cfg_attr") => {
                if let Ok(items) = list.parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                ) {
                    for item in items {
                        self.attribute(&item);
                    }
                } else {
                    self.invalid = true;
                }
            }
            _ => {}
        }
    }
}
impl<'ast> Visit<'ast> for Modules<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        for attribute in &node.attrs {
            self.attribute(&attribute.meta);
        }
        let previous = self.directory.clone();
        if node.content.is_some() {
            self.directory.push(node.ident.to_string());
        }
        visit::visit_item_mod(self, node);
        self.directory = previous;
    }
}
