//! The languages outlines know: their files, grammars, and which nodes are
//! declarations.
//!
//! `kinds` are node kinds that are always a declaration. `query` holds
//! patterns for the rest, capturing the declaration as `@item`; a grammar's
//! own tags query (`@definition.…` captures) is used where it's good, after
//! ours. Every kind and pattern is checked against its grammar by the tests.

use std::collections::HashSet;
use std::sync::OnceLock;
use tree_sitter::{Language, Query};
use tree_sitter_language::LanguageFn;

pub struct Lang {
    pub name: &'static str,
    extensions: &'static [&'static str],
    /// Whole file names (Makefile, BUILD…), matched case-sensitively.
    file_names: &'static [&'static str],
    language: LanguageFn,
    kinds: &'static [&'static str],
    query: &'static str,
    tags: Option<&'static str>,
    cache: OnceLock<Cache>,
}

struct Cache {
    kind_ids: HashSet<u16>,
    query: Option<Query>,
}

impl Lang {
    pub fn grammar(&self) -> Language {
        self.language.into()
    }

    fn cache(&self) -> &Cache {
        self.cache.get_or_init(|| {
            let grammar = self.grammar();
            let kind_ids = self
                .kinds
                .iter()
                .map(|k| grammar.id_for_node_kind(k, true))
                .filter(|&id| id != 0)
                .collect();
            let source = self.query_source();
            let query = if source.trim().is_empty() { None } else { Query::new(&grammar, &source).ok() };
            Cache { kind_ids, query }
        })
    }

    pub fn kind_ids(&self) -> &HashSet<u16> {
        &self.cache().kind_ids
    }

    pub fn query(&self) -> Option<&Query> {
        self.cache().query.as_ref()
    }

    pub(super) fn query_source(&self) -> String {
        format!("{}\n{}", self.query, self.tags.unwrap_or(""))
    }

    #[cfg(test)]
    pub(super) fn kinds(&self) -> &'static [&'static str] {
        self.kinds
    }

    /// A file name in this language.
    #[cfg(test)]
    pub(super) fn sample_name(&self) -> String {
        match self.extensions.first() {
            Some(ext) => format!("sample.{ext}"),
            None => self.file_names[0].to_string(),
        }
    }
}

/// The language of the file at `path`, by its name or extension. An
/// extension several languages use (`.h`, `.m`) is told apart by `content`.
pub fn language_for(path: &str, content: &str) -> Option<&'static Lang> {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if let Some(lang) = LANGUAGES.iter().find(|l| l.file_names.contains(&file)) {
        return Some(lang);
    }
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let ext = ext.to_ascii_lowercase();
    let name = match ext.as_str() {
        "h" if looks_objc(content) => Some("Objective-C"),
        "h" if looks_cpp(content) => Some("C++"),
        "m" if looks_objc(content) => Some("Objective-C"),
        "m" => Some("MATLAB"),
        _ => None,
    };
    match name {
        Some(name) => LANGUAGES.iter().find(|l| l.name == name),
        None => LANGUAGES.iter().find(|l| l.extensions.contains(&ext.as_str())),
    }
}

/// Lines that start with one of `marks`, ignoring indentation.
fn has_line_starting(content: &str, marks: &[&str]) -> bool {
    content.lines().take(5_000).any(|l| {
        let l = l.trim_start();
        marks.iter().any(|m| l.starts_with(m))
    })
}

fn looks_objc(content: &str) -> bool {
    has_line_starting(content, &["@interface", "@implementation", "@protocol", "#import", "@property", "@end"])
}

fn looks_cpp(content: &str) -> bool {
    has_line_starting(content, &["class ", "namespace ", "template<", "template <", "public:", "private:", "protected:", "using namespace"])
        || content.contains("std::")
}

#[cfg(test)]
pub(super) fn all() -> &'static [Lang] {
    &LANGUAGES
}

macro_rules! lang {
    ($name:literal, $lang:expr, ext: [$($ext:literal),*], files: [$($file:literal),*], kinds: [$($kind:literal),*], query: $query:expr, tags: $tags:expr) => {
        Lang {
            name: $name,
            extensions: &[$($ext),*],
            file_names: &[$($file),*],
            language: $lang,
            kinds: &[$($kind),*],
            query: $query,
            tags: $tags,
            cache: OnceLock::new(),
        }
    };
}

// ---- shared kinds and patterns ----

const JS_QUERY: &str = r#"
(program [(lexical_declaration) (variable_declaration)] @item)
(program (export_statement declaration: [(lexical_declaration) (variable_declaration)] @item))
(lexical_declaration (variable_declarator value: [(arrow_function) (function_expression) (generator_function) (class)])) @item
(variable_declaration (variable_declarator value: [(arrow_function) (function_expression) (generator_function) (class)])) @item
(field_definition value: [(arrow_function) (function_expression)]) @item
(expression_statement (assignment_expression
  left: (member_expression)
  right: [(arrow_function) (function_expression) (class)])) @item
"#;

const TS_QUERY: &str = r#"
(program [(lexical_declaration) (variable_declaration)] @item)
(program (export_statement declaration: [(lexical_declaration) (variable_declaration)] @item))
(lexical_declaration (variable_declarator value: [(arrow_function) (function_expression) (generator_function) (class)])) @item
(variable_declaration (variable_declarator value: [(arrow_function) (function_expression) (generator_function) (class)])) @item
(public_field_definition value: [(arrow_function) (function_expression)]) @item
(expression_statement (assignment_expression
  left: (member_expression)
  right: [(arrow_function) (function_expression) (class)])) @item
"#;

const C_QUERY: &str = r#"
(struct_specifier body: (_)) @item
(union_specifier body: (_)) @item
(enum_specifier body: (_)) @item
(declaration declarator: [
  (function_declarator)
  (pointer_declarator declarator: (function_declarator))
  (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator)))
]) @item
"#;

const CPP_QUERY: &str = r#"
(struct_specifier body: (_)) @item
(union_specifier body: (_)) @item
(enum_specifier body: (_)) @item
(class_specifier body: (_)) @item
(declaration declarator: [
  (function_declarator)
  (pointer_declarator declarator: (function_declarator))
  (reference_declarator (function_declarator))
]) @item
(field_declaration declarator: [
  (function_declarator)
  (pointer_declarator declarator: (function_declarator))
  (reference_declarator (function_declarator))
]) @item
"#;

static LANGUAGES: [Lang; 72] = [
    lang!("Rust", tree_sitter_rust::LANGUAGE, ext: ["rs"], files: [],
        kinds: ["function_item", "function_signature_item", "impl_item", "trait_item", "struct_item", "enum_item",
            "union_item", "mod_item", "type_item", "const_item", "static_item", "macro_definition", "foreign_mod_item"],
        query: "(impl_item type: (_) @name) @item", tags: None),
    lang!("JavaScript", tree_sitter_javascript::LANGUAGE, ext: ["js", "jsx", "mjs", "cjs"], files: [],
        kinds: ["function_declaration", "generator_function_declaration", "class_declaration", "class", "method_definition"],
        query: JS_QUERY, tags: None),
    lang!("TypeScript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT, ext: ["ts", "mts", "cts"], files: [],
        kinds: ["function_declaration", "generator_function_declaration", "class_declaration", "class", "abstract_class_declaration",
            "method_definition", "interface_declaration", "type_alias_declaration", "enum_declaration", "internal_module",
            "module", "function_signature", "method_signature", "abstract_method_signature"],
        query: TS_QUERY, tags: None),
    lang!("TSX", tree_sitter_typescript::LANGUAGE_TSX, ext: ["tsx"], files: [],
        kinds: ["function_declaration", "generator_function_declaration", "class_declaration", "class", "abstract_class_declaration",
            "method_definition", "interface_declaration", "type_alias_declaration", "enum_declaration", "internal_module",
            "module", "function_signature", "method_signature", "abstract_method_signature"],
        query: TS_QUERY, tags: None),
    lang!("Python", tree_sitter_python::LANGUAGE, ext: ["py", "pyi", "pyw"], files: [],
        kinds: ["function_definition", "class_definition", "type_alias_statement"],
        query: "(module (expression_statement (assignment left: (identifier))) @item)", tags: None),
    lang!("Go", tree_sitter_go::LANGUAGE, ext: ["go"], files: [],
        kinds: ["function_declaration", "method_declaration", "type_spec", "type_alias", "method_elem"],
        query: "(source_file (const_declaration (const_spec) @item)) (source_file (var_declaration (var_spec) @item))",
        tags: None),
    lang!("Java", tree_sitter_java::LANGUAGE, ext: ["java"], files: [],
        kinds: ["class_declaration", "interface_declaration", "enum_declaration", "record_declaration",
            "annotation_type_declaration", "method_declaration", "constructor_declaration",
            "compact_constructor_declaration", "module_declaration"],
        query: "", tags: None),
    lang!("C", tree_sitter_c::LANGUAGE, ext: ["c", "h"], files: [],
        kinds: ["function_definition", "type_definition", "preproc_function_def"],
        query: C_QUERY, tags: None),
    lang!("C++", tree_sitter_cpp::LANGUAGE, ext: ["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ipp", "tpp", "inl", "ixx", "cppm"], files: [],
        kinds: ["function_definition", "type_definition", "namespace_definition", "alias_declaration", "concept_definition",
            "preproc_function_def"],
        query: CPP_QUERY, tags: None),
    lang!("CUDA", tree_sitter_cuda::LANGUAGE, ext: ["cu", "cuh"], files: [],
        kinds: ["function_definition", "type_definition", "namespace_definition", "alias_declaration", "concept_definition",
            "preproc_function_def"],
        query: CPP_QUERY, tags: None),
    lang!("C#", tree_sitter_c_sharp::LANGUAGE, ext: ["cs", "csx"], files: [],
        kinds: ["class_declaration", "struct_declaration", "interface_declaration", "enum_declaration", "record_declaration",
            "delegate_declaration", "namespace_declaration", "file_scoped_namespace_declaration", "method_declaration",
            "constructor_declaration", "destructor_declaration", "property_declaration", "indexer_declaration",
            "event_declaration", "operator_declaration", "conversion_operator_declaration", "local_function_statement"],
        query: "", tags: None),
    lang!("Ruby", tree_sitter_ruby::LANGUAGE, ext: ["rb", "rake", "gemspec", "ru"], files: ["Rakefile", "Gemfile", "Vagrantfile", "Guardfile", "Podfile", "Fastfile", "Brewfile"],
        kinds: ["class", "module", "method", "singleton_method", "singleton_class"],
        query: "(program (assignment left: (constant)) @item)", tags: None),
    lang!("PHP", tree_sitter_php::LANGUAGE_PHP, ext: ["php", "phtml", "php3", "php4", "php5", "php7", "phps"], files: [],
        kinds: ["class_declaration", "interface_declaration", "trait_declaration", "enum_declaration", "function_definition",
            "method_declaration", "namespace_definition"],
        query: "", tags: None),
    lang!("Kotlin", tree_sitter_kotlin_ng::LANGUAGE, ext: ["kt", "kts"], files: [],
        kinds: ["class_declaration", "object_declaration", "function_declaration", "companion_object", "type_alias",
            "secondary_constructor"],
        query: "(source_file (property_declaration) @item) (class_body (property_declaration) @item)", tags: None),
    lang!("Swift", tree_sitter_swift::LANGUAGE, ext: ["swift"], files: [],
        kinds: ["class_declaration", "protocol_declaration", "function_declaration", "init_declaration", "deinit_declaration",
            "subscript_declaration", "typealias_declaration", "protocol_function_declaration", "operator_declaration"],
        query: "(source_file (property_declaration) @item) (class_body (property_declaration) @item) (protocol_body (protocol_property_declaration) @item)",
        tags: None),
    lang!("Scala", tree_sitter_scala::LANGUAGE, ext: ["scala", "sc", "sbt"], files: [],
        kinds: ["class_definition", "object_definition", "trait_definition", "enum_definition", "function_definition",
            "function_declaration", "type_definition", "given_definition", "extension_definition", "package_object"],
        query: "(compilation_unit [(val_definition) (var_definition)] @item) (template_body [(val_definition) (var_definition) (val_declaration) (var_declaration)] @item)",
        tags: None),
    lang!("Lua", tree_sitter_lua::LANGUAGE, ext: ["lua"], files: [],
        kinds: ["function_declaration"],
        query: "(chunk (variable_declaration) @item) (assignment_statement (expression_list value: (function_definition))) @item",
        tags: None),
    lang!("Bash", tree_sitter_bash::LANGUAGE, ext: ["sh", "bash", "zsh", "ksh", "bats"], files: [".bashrc", ".bash_profile", ".zshrc", ".profile", "PKGBUILD"],
        kinds: ["function_definition"],
        query: "", tags: None),
    lang!("Haskell", tree_sitter_haskell::LANGUAGE, ext: ["hs", "hs-boot", "lhs"], files: [],
        kinds: ["function", "bind", "signature", "data_type", "newtype", "type_synonym", "type_family", "class", "instance",
            "foreign_import", "pattern_synonym"],
        query: "", tags: None),
    lang!("OCaml", tree_sitter_ocaml::LANGUAGE_OCAML, ext: ["ml", "mll", "mly"], files: [],
        kinds: ["value_definition", "type_definition", "module_definition", "module_type_definition", "class_definition",
            "class_type_definition", "exception_definition", "external", "method_definition"],
        query: "", tags: None),
    lang!("OCaml interface", tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE, ext: ["mli"], files: [],
        kinds: ["value_specification", "type_definition", "module_definition", "module_type_definition",
            "class_definition", "class_type_definition", "exception_definition", "external", "method_specification"],
        query: "", tags: None),
    lang!("Elixir", tree_sitter_elixir::LANGUAGE, ext: ["ex", "exs"], files: [],
        kinds: [],
        query: r#"
            (call target: (identifier) @_kw
              (#any-of? @_kw "defmodule" "def" "defp" "defmacro" "defmacrop" "defguard" "defguardp" "defdelegate"
                "defstruct" "defprotocol" "defimpl" "defexception" "defoverridable" "describe" "test")) @item
            (call target: (identifier) @_kw
              (#any-of? @_kw "defmodule" "defprotocol" "defimpl" "def" "defp" "defmacro" "defmacrop" "defguard"
                "defguardp" "defdelegate" "describe" "test")
              (arguments . [
                (alias) @name
                (identifier) @name
                (string) @name
                (call target: (identifier) @name)
                (binary_operator left: (call target: (identifier) @name))
              ])) @item
        "#,
        tags: None),
    lang!("Erlang", tree_sitter_erlang::LANGUAGE, ext: ["erl", "hrl", "escript"], files: ["rebar.config"],
        kinds: ["fun_decl", "record_decl", "type_alias", "opaque", "pp_define", "spec", "callback"],
        query: "", tags: None),
    lang!("Dart", tree_sitter_dart::LANGUAGE, ext: ["dart"], files: [],
        kinds: ["class_declaration", "mixin_declaration", "extension_declaration", "enum_declaration", "type_alias",
            "extension_type_declaration", "function_declaration", "getter_declaration", "setter_declaration",
            "method_declaration", "local_function_declaration"],
        query: "(class_member (declaration) @item) (source_file (top_level_variable_declaration) @item)",
        tags: None),
    lang!("Zig", tree_sitter_zig::LANGUAGE, ext: ["zig", "zon"], files: [],
        kinds: ["function_declaration", "test_declaration"],
        query: "(source_file (variable_declaration) @item) (struct_declaration (variable_declaration) @item) (union_declaration (variable_declaration) @item) (enum_declaration (variable_declaration) @item) (opaque_declaration (variable_declaration) @item)",
        tags: None),
    lang!("Julia", tree_sitter_julia::LANGUAGE, ext: ["jl"], files: [],
        kinds: ["function_definition", "macro_definition", "struct_definition", "abstract_definition", "primitive_definition",
            "module_definition"],
        query: "(source_file (const_statement) @item) (module_definition (const_statement) @item) (source_file (assignment (call_expression)) @item) (module_definition (assignment (call_expression)) @item)",
        tags: None),
    lang!("R", tree_sitter_r::LANGUAGE, ext: ["r", "rprofile"], files: [".Rprofile"],
        kinds: [],
        query: r#"(binary_operator lhs: (identifier) rhs: (function_definition)) @item"#,
        tags: None),
    lang!("Objective-C", tree_sitter_objc::LANGUAGE, ext: ["m", "mm"], files: [],
        kinds: ["function_definition", "type_definition", "class_interface", "class_implementation", "protocol_declaration",
            "method_declaration", "method_definition", "property_declaration", "preproc_function_def"],
        query: C_QUERY, tags: None),
    lang!("Elm", tree_sitter_elm::LANGUAGE, ext: ["elm"], files: [],
        kinds: ["value_declaration", "type_declaration", "type_alias_declaration", "type_annotation", "port_annotation",
            "infix_declaration"],
        query: "(value_declaration (function_declaration_left . (lower_case_identifier) @name)) @item", tags: None),
    lang!("HCL", tree_sitter_hcl::LANGUAGE, ext: ["hcl", "tf", "tfvars", "nomad"], files: [],
        kinds: ["block"],
        query: "(config_file (body (attribute) @item))", tags: None),
    lang!("Solidity", tree_sitter_solidity::LANGUAGE, ext: ["sol"], files: [],
        kinds: ["contract_declaration", "interface_declaration", "library_declaration", "struct_declaration",
            "enum_declaration", "function_definition", "modifier_definition", "event_definition", "error_declaration",
            "constructor_definition", "fallback_receive_definition", "state_variable_declaration",
            "user_defined_type_definition"],
        query: "", tags: None),
    lang!("Nix", tree_sitter_nix::LANGUAGE, ext: ["nix"], files: [],
        kinds: ["binding"],
        query: "", tags: None),
    lang!("CSS", tree_sitter_css::LANGUAGE, ext: ["css", "pcss", "postcss"], files: [],
        kinds: ["rule_set", "media_statement", "keyframes_statement", "supports_statement", "at_rule"],
        query: "", tags: None),
    lang!("HTML", tree_sitter_html::LANGUAGE, ext: ["html", "htm", "xhtml"], files: [],
        kinds: ["script_element", "style_element"],
        query: r#"
            (element (start_tag (tag_name) @_tag)
              (#any-of? @_tag "head" "body" "header" "nav" "main" "section" "article" "aside" "footer" "form" "table"
                "template" "dialog" "h1" "h2" "h3" "h4" "h5" "h6")) @item
            (element (start_tag (attribute (attribute_name) @_attr (#eq? @_attr "id")))) @item
        "#,
        tags: None),
    lang!("JSON", tree_sitter_json::LANGUAGE, ext: ["json", "jsonc", "json5", "geojson", "webmanifest", "har"], files: [".babelrc", ".eslintrc", ".prettierrc"],
        kinds: ["pair"],
        query: "", tags: None),
    lang!("YAML", tree_sitter_yaml::LANGUAGE, ext: ["yaml", "yml"], files: [".clang-format", ".clangd"],
        kinds: ["block_mapping_pair"],
        query: "", tags: None),
    lang!("TOML", tree_sitter_toml_ng::LANGUAGE, ext: ["toml"], files: ["Cargo.lock", "Pipfile", "poetry.lock"],
        kinds: ["table", "table_array_element"],
        query: "(document (pair) @item)", tags: None),
    lang!("Markdown", tree_sitter_md::LANGUAGE, ext: ["md", "markdown", "mdx", "mkd", "mdown"], files: [],
        kinds: [],
        // A section is a declaration when a heading starts it (text before
        // the first heading is a section too, without one).
        query: "(section . [(atx_heading) (setext_heading)]) @item", tags: None),
    lang!("Protocol Buffers", tree_sitter_proto::LANGUAGE, ext: ["proto"], files: [],
        kinds: ["message", "enum", "service", "rpc", "extend", "oneof"],
        query: "", tags: None),
    lang!("Gleam", tree_sitter_gleam::LANGUAGE, ext: ["gleam"], files: [],
        kinds: ["function", "type_definition", "type_alias", "constant", "external_function", "external_type"],
        query: "", tags: None),
    lang!("F#", tree_sitter_fsharp::LANGUAGE_FSHARP, ext: ["fs", "fsx"], files: [],
        kinds: ["function_or_value_defn", "type_definition", "module_defn", "namespace", "named_module",
            "exception_definition", "member_defn"],
        query: "", tags: None),
    lang!("F# signature", tree_sitter_fsharp::LANGUAGE_SIGNATURE, ext: ["fsi"], files: [],
        kinds: ["value_definition", "type_definition", "module_defn", "namespace", "named_module", "exception_definition",
            "member_signature"],
        query: "", tags: None),
    lang!("Groovy", tree_sitter_groovy::LANGUAGE, ext: ["groovy", "gradle", "gvy", "gy", "gsh"], files: ["Jenkinsfile"],
        kinds: ["class_declaration", "interface_declaration", "enum_declaration", "record_declaration",
            "annotation_type_declaration", "method_declaration", "constructor_declaration", "function_definition"],
        query: "", tags: None),
    lang!("PowerShell", tree_sitter_powershell::LANGUAGE, ext: ["ps1", "psm1", "psd1"], files: [],
        kinds: ["function_statement", "class_statement", "class_method_definition", "class_property_definition",
            "enum_statement"],
        query: "", tags: None),
    lang!("Common Lisp", tree_sitter_commonlisp::LANGUAGE_COMMONLISP, ext: ["lisp", "lsp", "cl", "asd"], files: [],
        kinds: [],
        query: r#"
            (list_lit (defun (defun_header function_name: (_) @name))) @item
            (list_lit . (sym_lit) @_kw
              (#match? @_kw "(?i)^(defpackage|defvar|defparameter|defconstant|defclass|defstruct|defgeneric|deftype|define-condition|defsystem|define-compiler-macro|defsetf)$")
              . [(sym_lit) (kwd_lit) (list_lit)] @name) @item
        "#, tags: None),
    lang!("Racket", tree_sitter_racket::LANGUAGE, ext: ["rkt", "rktl", "rktd"], files: [],
        kinds: [],
        query: r#"
            (list . (symbol) @_kw
              (#match? @_kw "^(define|define-values|define-syntax|define-syntax-rule|define-record-type|define-struct|struct|module|module\\+|module\\*|define/contract|define-type|define-library|define-module|define-public|define-method|define-generic|define-class)$")
              . [(symbol) @name (list . (symbol) @name)]) @item
        "#, tags: None),
    lang!("Scheme", tree_sitter_scheme::LANGUAGE, ext: ["scm", "ss", "sld", "sls"], files: [],
        kinds: [],
        query: r#"
            (list . (symbol) @_kw
              (#match? @_kw "^(define|define-values|define-syntax|define-syntax-rule|define-record-type|define-struct|struct|module|module\\+|module\\*|define/contract|define-type|define-library|define-module|define-public|define-method|define-generic|define-class)$")
              . [(symbol) @name (list . (symbol) @name)]) @item
        "#, tags: None),
    lang!("Emacs Lisp", tree_sitter_elisp::LANGUAGE, ext: ["el"], files: [".emacs"],
        kinds: ["function_definition", "macro_definition"],
        query: r#"
            (special_form . ["defvar" "defconst"] . (symbol) @name) @item
            (list . (symbol) @_kw
              (#any-of? @_kw "defcustom" "defface" "defgroup" "defsubst" "defalias" "cl-defun" "cl-defmacro" "cl-defmethod"
                "cl-defgeneric" "cl-defstruct" "define-minor-mode" "define-derived-mode" "define-globalized-minor-mode"
                "use-package" "ert-deftest")
              . (_) @name) @item
        "#, tags: None),
    lang!("Fortran", tree_sitter_fortran::LANGUAGE, ext: ["f", "f90", "f95", "f03", "f08", "for", "f77", "fpp"], files: [],
        kinds: ["program", "module", "submodule", "function", "subroutine", "interface", "derived_type_definition",
            "module_procedure"],
        query: "", tags: None),
    lang!("Ada", tree_sitter_ada::LANGUAGE, ext: ["adb", "ads", "ada"], files: [],
        kinds: ["subprogram_body", "subprogram_declaration", "expression_function_declaration", "package_declaration",
            "package_body", "full_type_declaration", "task_body", "protected_body", "generic_package_declaration",
            "generic_subprogram_declaration"],
        query: "", tags: None),
    lang!("Pascal", tree_sitter_pascal::LANGUAGE, ext: ["pas", "lpr", "dpr", "dpk"], files: [],
        kinds: ["defProc", "declProc", "declType", "declClass", "unit", "program"],
        query: "", tags: None),
    lang!("VHDL", tree_sitter_vhdl::LANGUAGE, ext: ["vhd", "vhdl"], files: [],
        kinds: ["entity_declaration", "architecture_definition", "package_declaration", "package_definition",
            "component_declaration", "subprogram_definition", "subprogram_declaration", "process_statement",
            "configuration_declaration", "type_declaration"],
        query: "", tags: None),
    lang!("Verilog", tree_sitter_verilog::LANGUAGE, ext: ["v", "vh"], files: [],
        kinds: ["module_declaration", "interface_declaration", "package_declaration", "class_declaration",
            "function_declaration", "task_declaration", "program_declaration", "checker_declaration"],
        query: "", tags: None),
    lang!("SystemVerilog", tree_sitter_systemverilog::LANGUAGE, ext: ["sv", "svh"], files: [],
        kinds: ["module_declaration", "interface_declaration", "package_declaration", "class_declaration",
            "function_declaration", "task_declaration", "program_declaration", "checker_declaration",
            "covergroup_declaration", "class_constructor_declaration"],
        query: "", tags: None),
    lang!("D", tree_sitter_d::LANGUAGE, ext: ["d", "di"], files: [],
        kinds: ["function_declaration", "class_declaration", "struct_declaration", "interface_declaration",
            "enum_declaration", "union_declaration", "template_declaration", "module_declaration", "constructor",
            "destructor", "unittest_declaration", "alias_declaration", "mixin_template_declaration"],
        query: "", tags: None),
    lang!("Nim", tree_sitter_nim::LANGUAGE, ext: ["nim", "nims", "nimble"], files: [],
        kinds: ["proc_declaration", "func_declaration"],
        query: "", tags: None),
    lang!("Crystal", tree_sitter_crystal::LANGUAGE, ext: ["cr"], files: [],
        kinds: ["class_declaration", "method_definition"],
        query: "", tags: None),
    lang!("Odin", tree_sitter_odin::LANGUAGE, ext: ["odin"], files: [],
        kinds: ["procedure_declaration", "overloaded_procedure_declaration", "struct_declaration", "enum_declaration",
            "union_declaration", "bit_field_declaration", "const_declaration"],
        query: "", tags: None),
    lang!("GLSL", tree_sitter_glsl::LANGUAGE_GLSL, ext: ["glsl", "vert", "frag", "geom", "comp", "tesc", "tese", "mesh", "task", "rgen", "rchit", "rmiss"], files: [],
        kinds: ["function_definition", "type_definition", "preproc_function_def"],
        query: C_QUERY, tags: None),
    lang!("HLSL", tree_sitter_hlsl::LANGUAGE_HLSL, ext: ["hlsl", "hlsli", "fx", "fxh"], files: [],
        kinds: ["function_definition", "type_definition", "namespace_definition", "preproc_function_def"],
        query: CPP_QUERY, tags: None),
    lang!("Make", tree_sitter_make::LANGUAGE, ext: ["mk", "mak", "make"], files: ["Makefile", "makefile", "GNUmakefile"],
        kinds: ["rule", "define_directive"],
        query: "(makefile (variable_assignment) @item)", tags: None),
    lang!("CMake", tree_sitter_cmake::LANGUAGE, ext: ["cmake"], files: ["CMakeLists.txt"],
        kinds: ["function_def", "macro_def"],
        query: "", tags: None),
    lang!("GraphQL", tree_sitter_graphql::LANGUAGE, ext: ["graphql", "graphqls", "gql"], files: [],
        kinds: ["object_type_definition", "interface_type_definition", "enum_type_definition",
            "input_object_type_definition", "union_type_definition", "scalar_type_definition", "directive_definition",
            "schema_definition", "operation_definition", "fragment_definition", "object_type_extension",
            "interface_type_extension", "enum_type_extension", "input_object_type_extension", "union_type_extension",
            "scalar_type_extension", "schema_extension", "field_definition"],
        query: "", tags: None),
    lang!("MATLAB", tree_sitter_matlab::LANGUAGE, ext: ["matlab", "m"], files: [],
        kinds: ["function_definition", "class_definition", "properties", "methods", "events", "enumeration"],
        query: "", tags: None),
    lang!("Starlark", tree_sitter_starlark::LANGUAGE, ext: ["bzl", "star", "bazel", "sky"], files: ["BUILD", "WORKSPACE", "MODULE.bazel", "Tiltfile", "BUCK"],
        kinds: ["function_definition"],
        query: "(module (expression_statement (call)) @item) (module (expression_statement (assignment left: (identifier))) @item)",
        tags: None),
    lang!("Nickel", tree_sitter_nickel::LANGUAGE, ext: ["ncl"], files: [],
        kinds: ["let_binding", "field_def"],
        query: "(let_binding pat: (_) @name) @item (field_def path: (_) @name) @item", tags: None),
    lang!("Agda", tree_sitter_agda::LANGUAGE, ext: ["agda"], files: [],
        kinds: ["module", "data", "record", "function", "postulate", "signature"],
        query: "", tags: None),
    lang!("XML", tree_sitter_xml::LANGUAGE_XML, ext: ["xml", "xsd", "xsl", "xslt", "svg", "plist", "csproj", "fsproj", "vbproj", "props", "targets", "resx", "xaml", "wsdl", "pom", "rss", "atom"], files: [],
        kinds: ["element"],
        query: "", tags: None),
    lang!("Puppet", tree_sitter_puppet::LANGUAGE, ext: ["pp"], files: [],
        kinds: ["class_definition", "defined_resource_type", "function_declaration", "node_definition", "type_declaration"],
        query: "", tags: None),
    lang!("KDL", tree_sitter_kdl::LANGUAGE, ext: ["kdl"], files: [],
        kinds: ["node"],
        query: "", tags: None),
    lang!("Perl", tree_sitter_perl::LANGUAGE, ext: ["pl", "pm", "t", "psgi"], files: [],
        kinds: ["function_definition", "package_statement"],
        query: "", tags: None),
    lang!("SPARQL", tree_sitter_sparql::LANGUAGE, ext: ["rq", "sparql"], files: [],
        kinds: ["SelectQuery", "ConstructQuery", "DescribeQuery", "AskQuery", "Update1"],
        query: "", tags: None),
];
