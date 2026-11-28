use std::collections::BTreeMap;

use concir_ir::ast::{BaseType, ComplexBaseType};
use concir_ir::expr::{BinOp, CmpOp};
use concir_sem::eval::LExpr;
use concir_sem::ids::{ModuleId, ResourceId, SlotRef};
use concir_sem::program::{ResKind, SemProgram, SemTypeDef};
use concir_sem::value::Value;

use crate::{
    default_expr, has_large_array, program_needs_domain, render_lit, rust_ty, sanitize,
    struct_key, Gen, Hole, SidLocation, TypeHole,
};

impl<'a> Gen<'a> {
    pub(crate) fn new(program: &'a SemProgram) -> Self {
        let mut name_counts: BTreeMap<String, usize> = BTreeMap::new();
        for t in program.types() {
            *name_counts.entry(sanitize(&t.name)).or_insert(0) += 1;
        }
        let mut named_types = BTreeMap::new();
        for t in program.types() {
            let short = sanitize(&t.name);
            let rust_name = if name_counts.get(&short).copied().unwrap_or(0) > 1 {
                format!("{}_{}", sanitize(program.module_name(t.module)), short)
            } else {
                short
            };
            named_types.insert((t.module, t.name.clone()), rust_name);
        }
        Gen {
            program,
            lines: Vec::new(),
            current_file: "src/main.rs".to_string(),
            sids: BTreeMap::new(),
            holes: Vec::new(),
            type_holes: Vec::new(),
            tags: Vec::new(),
            hole_seq: 0,
            named_types,
            anon_structs: BTreeMap::new(),
            domain_lines: Vec::new(),
        }
    }

    pub(crate) fn resolve_rust_ty(
        &mut self,
        from_module: ModuleId,
        raw_ty: Option<&BaseType>,
        resolved_ty: &BaseType,
        in_domain_mod: bool,
    ) -> Result<String, String> {
        let prefix = if in_domain_mod { "" } else { "crate::domain::" };
        if let Some(BaseType::Primitive(p)) = raw_ty {
            if !concir_ir::typedef::is_builtin(p) {
                if let Some(td) = self.program.resolve_type_def(from_module, p) {
                    if let Some(rname) = self.named_types.get(&(td.module, td.name.clone())) {
                        return Ok(format!("{prefix}{rname}"));
                    }
                }
            }
        }
        match resolved_ty {
            BaseType::Primitive(p) => match p.as_str() {
                "Bool" => Ok("bool".into()),
                "Int" => Ok("i64".into()),
                "Float" => Ok("f64".into()),
                "String" => Ok("String".into()),
                other => {
                    if let Some(td) = self.program.resolve_type_def(from_module, other) {
                        if let Some(rname) = self.named_types.get(&(td.module, td.name.clone())) {
                            return Ok(format!("{prefix}{rname}"));
                        }
                    }
                    Err(format!("unsupported primitive type '{other}'"))
                }
            },
            BaseType::Complex(ComplexBaseType::BoundedInt { .. }) => Ok("i64".into()),
            BaseType::Complex(ComplexBaseType::Enum(_)) => Ok("String".into()),
            BaseType::Complex(ComplexBaseType::Array(def)) => {
                let raw_elem = match raw_ty {
                    Some(BaseType::Complex(ComplexBaseType::Array(rdef))) => Some(&rdef.elem),
                    _ => None,
                };
                let elem = self.resolve_rust_ty(from_module, raw_elem, &def.elem, in_domain_mod)?;
                Ok(format!("[{elem}; {}]", def.len.max(0)))
            }
            BaseType::Complex(ComplexBaseType::Struct(fields)) => {
                let key = struct_key(fields);
                let name = if let Some((existing, _)) = self.anon_structs.get(&key) {
                    existing.clone()
                } else {
                    let idx = self.anon_structs.len() + 1;
                    let nm = format!("AnonStruct{idx}");
                    self.anon_structs
                        .insert(key, (nm.clone(), fields.clone()));
                    nm
                };
                Ok(format!("{prefix}{name}"))
            }
            BaseType::Complex(ComplexBaseType::Hole { .. }) => Ok("()".into()),
        }
    }

    pub(crate) fn render_value_typed(
        &mut self,
        v: &Value,
        from_module: ModuleId,
        raw_ty: Option<&BaseType>,
        resolved_ty: &BaseType,
    ) -> Result<String, String> {
        if matches!(resolved_ty, BaseType::Complex(ComplexBaseType::Hole { .. })) {
            let ty = self.resolve_rust_ty(from_module, raw_ty, resolved_ty, false)?;
            return Ok(format!("{ty}::default()"));
        }
        Ok(match v {
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format!("{f:?}f64"),
            Value::Str(s) => format!("{s:?}.to_string()"),
            Value::Enum(s) => format!("{s:?}.to_string()"),
            Value::Struct(fields) => {
                let struct_ty = self.resolve_rust_ty(from_module, raw_ty, resolved_ty, false)?;
                let field_tys = match resolved_ty {
                    BaseType::Complex(ComplexBaseType::Struct(ft)) => Some(ft),
                    _ => None,
                };
                let raw_field_tys = match raw_ty {
                    Some(BaseType::Complex(ComplexBaseType::Struct(ft))) => Some(ft),
                    _ => None,
                };
                let mut parts = Vec::new();
                for (k, fv) in fields {
                    let fk = sanitize(k);
                    let f_res_ty = field_tys
                        .and_then(|m| m.get(k))
                        .cloned()
                        .unwrap_or_else(|| BaseType::Primitive("Int".into()));
                    let f_raw_ty = raw_field_tys.and_then(|m| m.get(k));
                    let rendered =
                        self.render_value_typed(fv, from_module, f_raw_ty, &f_res_ty)?;
                    parts.push(format!("{fk}: {rendered}"));
                }
                format!("{struct_ty} {{ {} }}", parts.join(", "))
            }
            Value::Array(items) => {
                let elem_res_ty = match resolved_ty {
                    BaseType::Complex(ComplexBaseType::Array(def)) => def.elem.clone(),
                    _ => BaseType::Primitive("Int".into()),
                };
                let elem_raw_ty = match raw_ty {
                    Some(BaseType::Complex(ComplexBaseType::Array(def))) => Some(&def.elem),
                    _ => None,
                };
                let mut parts = Vec::new();
                for item in items {
                    parts.push(self.render_value_typed(
                        item,
                        from_module,
                        elem_raw_ty,
                        &elem_res_ty,
                    )?);
                }
                format!("[{}]", parts.join(", "))
            }
        })
    }

    pub(crate) fn build_domain_rs(&mut self) -> Result<Option<String>, String> {
        let has_named = !self.program.types().is_empty();
        let has_anon = !self.anon_structs.is_empty();
        if !has_named && !has_anon {
            return Ok(None);
        }
        self.domain_lines.clear();
        self.domain_lines
            .push("// Generated domain types (`src/domain.rs`).".to_string());
        self.domain_lines.push("#![allow(dead_code)]".to_string());
        self.domain_lines.push(String::new());

        let mut th_seq = 0usize;
        let types: Vec<_> = self.program.types().to_vec();
        for t in &types {
            let mod_name = self.program.module_name(t.module).to_string();
            let rust_name = self
                .named_types
                .get(&(t.module, t.name.clone()))
                .cloned()
                .unwrap_or_else(|| sanitize(&t.name));
            match &t.ty {
                BaseType::Complex(ComplexBaseType::Hole { description, req }) => {
                    th_seq += 1;
                    let id = format!("th{th_seq}");
                    let line = self.domain_lines.len() + 1;
                    self.type_holes.push(TypeHole {
                        id: id.clone(),
                        type_name: rust_name.clone(),
                        module: mod_name,
                        file: "src/domain.rs".to_string(),
                        line,
                        description: description.clone(),
                        req: req.clone(),
                    });
                    self.domain_lines
                        .push(format!("/* TYPE_HOLE({id}) type: {rust_name} */"));
                    self.domain_lines
                        .push("#[derive(Clone, Debug, Default, PartialEq)]".to_string());
                    self.domain_lines.push(format!("pub struct {rust_name} {{}}"));
                    self.domain_lines.push(format!(
                        "const _: fn() = || {{ fn assert_domain_bounds<T: Send + Sync + Clone + Default + 'static>() {{}} assert_domain_bounds::<{rust_name}>(); }};"
                    ));
                    self.domain_lines.push(String::new());
                }
                BaseType::Complex(ComplexBaseType::Struct(fields)) => {
                    let raw_fields = match &t.raw_ty {
                        BaseType::Complex(ComplexBaseType::Struct(rf)) => Some(rf),
                        _ => None,
                    };
                    let large_arr = fields.values().any(has_large_array);
                    if large_arr {
                        self.domain_lines
                            .push("#[derive(Clone, Debug, PartialEq)]".to_string());
                    } else {
                        self.domain_lines
                            .push("#[derive(Clone, Debug, Default, PartialEq)]".to_string());
                    }
                    self.domain_lines.push(format!("pub struct {rust_name} {{"));
                    let mut default_fields = Vec::new();
                    for (fname, fty) in fields {
                        let raw_fty = raw_fields.and_then(|rf| rf.get(fname));
                        let rt = self.resolve_rust_ty(t.module, raw_fty, fty, true)?;
                        let sname = sanitize(fname);
                        self.domain_lines.push(format!("    pub {sname}: {rt},"));
                        default_fields.push((sname, default_expr(&rt)));
                    }
                    self.domain_lines.push("}".to_string());
                    if large_arr {
                        self.domain_lines
                            .push(format!("impl Default for {rust_name} {{"));
                        self.domain_lines.push("    fn default() -> Self {".to_string());
                        self.domain_lines.push("        Self {".to_string());
                        for (sname, dexpr) in default_fields {
                            self.domain_lines.push(format!("            {sname}: {dexpr},"));
                        }
                        self.domain_lines.push("        }".to_string());
                        self.domain_lines.push("    }".to_string());
                        self.domain_lines.push("}".to_string());
                    }
                    self.domain_lines.push(format!(
                        "const _: fn() = || {{ fn assert_domain_bounds<T: Send + Sync + Clone + Default + 'static>() {{}} assert_domain_bounds::<{rust_name}>(); }};"
                    ));
                    self.domain_lines.push(String::new());
                }
                other => {
                    let rt = self.resolve_rust_ty(t.module, None, other, true)?;
                    self.domain_lines.push(format!("pub type {rust_name} = {rt};"));
                    self.domain_lines.push(String::new());
                }
            }
        }

        // Emit any anonymous structs collected while lowering fields/resources/slots.
        let mut emitted_anon: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        loop {
            let pending: Vec<(String, BTreeMap<String, BaseType>)> = self
                .anon_structs
                .values()
                .filter(|(nm, _)| !emitted_anon.contains(nm))
                .cloned()
                .collect();
            if pending.is_empty() {
                break;
            }
            for (nm, fields) in pending {
                emitted_anon.insert(nm.clone());
                let large_arr = fields.values().any(has_large_array);
                if large_arr {
                    self.domain_lines
                        .push("#[derive(Clone, Debug, PartialEq)]".to_string());
                } else {
                    self.domain_lines
                        .push("#[derive(Clone, Debug, Default, PartialEq)]".to_string());
                }
                self.domain_lines.push(format!("pub struct {nm} {{"));
                let mut default_fields = Vec::new();
                for (fname, fty) in &fields {
                    let rt = self.resolve_rust_ty(ModuleId(0), None, fty, true)?;
                    let sname = sanitize(fname);
                    self.domain_lines.push(format!("    pub {sname}: {rt},"));
                    default_fields.push((sname, default_expr(&rt)));
                }
                self.domain_lines.push("}".to_string());
                if large_arr {
                    self.domain_lines.push(format!("impl Default for {nm} {{"));
                    self.domain_lines.push("    fn default() -> Self {".to_string());
                    self.domain_lines.push("        Self {".to_string());
                    for (sname, dexpr) in default_fields {
                        self.domain_lines.push(format!("            {sname}: {dexpr},"));
                    }
                    self.domain_lines.push("        }".to_string());
                    self.domain_lines.push("    }".to_string());
                    self.domain_lines.push("}".to_string());
                }
                self.domain_lines.push(format!(
                    "const _: fn() = || {{ fn assert_domain_bounds<T: Send + Sync + Clone + Default + 'static>() {{}} assert_domain_bounds::<{nm}>(); }};"
                ));
                self.domain_lines.push(String::new());
            }
        }

        Ok(Some(self.domain_lines.join("\n") + "\n"))
    }

    pub(crate) fn resource_payload_ty(&mut self, r: ResourceId) -> Result<String, String> {
        let res = self.program.resource(r).clone();
        match &res.ty {
            Some(bt) => self.resolve_rust_ty(res.module, res.raw_ty.as_ref(), bt, false),
            None => Ok("()".to_string()),
        }
    }

    pub(crate) fn line(&self) -> usize {
        self.lines.len() + 1
    }

    pub(crate) fn emit(&mut self, code: impl AsRef<str>) -> usize {
        let start = self.line();
        for l in code.as_ref().lines() {
            self.lines.push(l.to_string());
        }
        start
    }

    pub(crate) fn resource_fqn(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        concir_ir::fqn::fqn(self.program.module_name(res.module), &res.name)
    }

    pub(crate) fn function_fqn(&self, f: concir_sem::ids::FunctionId) -> String {
        let fun = self.program.function(f);
        concir_ir::fqn::fqn(self.program.module_name(fun.module), &fun.name)
    }

    pub(crate) fn field(&self, r: ResourceId) -> String {
        let res = self.program.resource(r);
        format!(
            "r_{}__{}",
            sanitize(self.program.module_name(res.module)),
            sanitize(&res.name)
        )
    }

    /// Cross-module path to a generated function.
    pub(crate) fn fn_ref(&self, f: concir_sem::ids::FunctionId) -> String {
        let func = self.program.function(f);
        format!(
            "crate::{}::cf_{}",
            sanitize(self.program.module_name(func.module)),
            sanitize(&func.name)
        )
    }

    #[allow(unused)]
    pub(crate) fn tag_expr(&self, tag: &str) -> String {
        format!("{tag:?}")
    }

    pub(crate) fn hole(&mut self, module: &str, function: &str, sid: &str, expected: &str) -> String {
        self.hole_seq += 1;
        let id = format!("h{}", self.hole_seq);
        let line = self.line();
        self.holes.push(Hole {
            id: id.clone(),
            module: module.to_string(),
            function: function.to_string(),
            sid: sid.to_string(),
            expected: expected.to_string(),
            file: self.current_file.clone(),
            line,
            reads: Vec::new(),
            writes: Vec::new(),
            req: Vec::new(),
        });
        format!("/* HOLE({id}) expected: {expected} */ Default::default()")
    }

    pub(crate) fn is_std_atomic(&self, r: ResourceId) -> bool {
        let res = self.program.resource(r);
        if res.kind != ResKind::Atomic {
            return false;
        }
        matches!(
            res.ty.as_ref().and_then(|bt| rust_ty(bt).ok()).as_deref(),
            Some("i64" | "bool")
        )
    }

    pub(crate) fn render_shared_read(&self, r: ResourceId) -> String {
        let f = self.field(r);
        let res = self.program.resource(r);
        match res.kind {
            ResKind::Var => format!("shared.{f}.read()"),
            ResKind::Atomic if self.is_std_atomic(r) => {
                format!("shared.{f}.load(std::sync::atomic::Ordering::SeqCst)")
            }
            _ => format!("shared.{f}.lock().unwrap().clone()"),
        }
    }

    pub(crate) fn render_shared_write(&self, r: ResourceId, val_expr: &str) -> String {
        let f = self.field(r);
        let res = self.program.resource(r);
        match res.kind {
            ResKind::Var => format!("shared.{f}.write({val_expr});"),
            ResKind::Atomic if self.is_std_atomic(r) => {
                format!("shared.{f}.store({val_expr}, std::sync::atomic::Ordering::SeqCst);")
            }
            _ => format!("*shared.{f}.lock().unwrap() = {val_expr};"),
        }
    }

    pub(crate) fn render_expr(&mut self, module: &str, function: &str, sid: &str, e: &LExpr) -> String {
        match e {
            LExpr::Lit(l) => match render_lit(l) {
                Ok(s) => s,
                Err(_) => self.hole(module, function, sid, "literal"),
            },
            LExpr::Slot(SlotRef::Local(i)) => format!("_l{i}.clone()"),
            LExpr::Slot(SlotRef::Discard) => "()".into(),
            LExpr::Slot(SlotRef::Shared(r)) => self.render_shared_read(*r),
            LExpr::Field { base, field } => {
                let b = self.render_expr(module, function, sid, base);
                format!("({b}).{}", sanitize(field))
            }
            LExpr::Neg(inner) => format!("(-{})", self.render_expr(module, function, sid, inner)),
            LExpr::Bin { op, lhs, rhs } => {
                let a = self.render_expr(module, function, sid, lhs);
                let b = self.render_expr(module, function, sid, rhs);
                let o = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Mod => "%",
                };
                format!("({a} {o} {b})")
            }
            LExpr::Cmp { op, lhs, rhs } => {
                let a = self.render_expr(module, function, sid, lhs);
                let b = self.render_expr(module, function, sid, rhs);
                let o = match op {
                    CmpOp::Eq => "==",
                    CmpOp::Ne => "!=",
                    CmpOp::Lt => "<",
                    CmpOp::Le => "<=",
                    CmpOp::Gt => ">",
                    CmpOp::Ge => ">=",
                };
                format!("({a} {o} {b})")
            }
            _ => self.hole(module, function, sid, "expression"),
        }
    }
}
