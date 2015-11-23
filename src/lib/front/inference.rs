// The MIT License (MIT)
//
// Copyright (c) 2015 Johan Johansson
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

//! Type inference

// TODO: Almost all `infer_types` takes const map + var stack + caller stack.
//       Maybe encapsulate this using some kind of state
// TODO: Replace verbose type error checks with:
//       `foo.infer_types(...)
//           .unwrap_or_else(foo.pos().type_mismatcher(expected_ty))`

use std::fmt::{ self, Display };
use std::mem::{ replace, swap };
use std::collections::HashMap;
use std::borrow::Cow;
use lib::front::ast::*;
use lib::collections::ScopeStack;
use self::InferenceErr::*;

enum InferenceErr<'p, 'src: 'p> {
	/// Type mismatch. (expected, found)
	TypeMis(&'p Type<'src>, &'p Type<'src>),
	ArmsDiffer(&'p Type<'src>, &'p Type<'src>),
}
impl<'src, 'p> Display for InferenceErr<'src, 'p> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			TypeMis(expected, found) =>
				write!(f, "Type mismatch. Expected `{}`, found `{}`", expected, found),
			ArmsDiffer(c, a) =>
				write!(f, "Consequent and alternative have different types. \
				           Expected `{}` from alternative, found `{}`",
					c,
					a),
		}
	}
}

struct Inferer<'src> {
	vars: Vec<(Ident<'src>, Type<'src>)>,
	const_defs: ScopeStack<Ident<'src>, Option<ConstDef<'src>>>,
	extern_funcs: ScopeStack<Ident<'src>, Type<'src>>,
}
impl<'src> Inferer<'src> {
	fn new(ast: &mut AST<'src>) -> Self {
		let mut const_defs = ScopeStack::new();
		const_defs.push(
			replace(&mut ast.const_defs, HashMap::new())
				.into_iter()
				.map(|(k, v)| (k, Some(v)))
				.collect());

		let mut extern_funcs = ScopeStack::new();
		extern_funcs.push(replace(&mut ast.extern_funcs, HashMap::new()));

		Inferer {
			vars: Vec::new(),
			const_defs: const_defs,
			extern_funcs: extern_funcs,
		}
	}

	fn into_inner(mut self)
		-> (HashMap<Ident<'src>, ConstDef<'src>>, HashMap<Ident<'src>, Type<'src>>)
	{
		let const_defs = self.const_defs.pop()
			.expect("ICE: Inferer::into_inner: const_defs.pop() failed")
			.into_iter()
			.map(|(k, v)| (k, v.expect("ICE: Inferer::into_inner: None when unmapping const def")))
			.collect();
		let extern_funcs = self.extern_funcs.pop()
			.expect("ICE: Inferer::into_inner: extern_funcs.pop() failed");

		(const_defs, extern_funcs)
	}

	fn get_var_type(&self, id: &str) -> Option<&Type<'src>> {
		self.vars.iter().rev().find(|&&(ref b, _)| b == id).map(|&(_, ref t)| t)
	}

	fn get_var_type_mut(&mut self, id: &str) -> Option<&mut Type<'src>> {
		self.vars.iter_mut().rev().find(|&&mut (ref b, _)| b == id).map(|&mut (_, ref mut t)| t)
	}

	fn infer_const_def(&mut self, def: &mut ConstDef<'src>, expected_ty: &Type<'src>)
		-> Type<'src>
	{
		self.infer_expr(&mut def.body, expected_ty)
	}

	fn infer_nil(&mut self, nil: &mut Nil<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		if let Some(type_nil) = expected_ty.infer_by(&TYPE_NIL) {
			nil.typ = type_nil.clone();
			type_nil
		} else {
			nil.pos.error(TypeMis(expected_ty, &TYPE_NIL))
		}
	}

	fn infer_num_lit(&mut self, lit: &mut NumLit<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		match *expected_ty {
			Type::Unknown
			| Type::Basic("Int8") | Type::Basic("UInt8")
			| Type::Basic("Int16") | Type::Basic("UInt16")
			| Type::Basic("Int32") | Type::Basic("UInt32") | Type::Basic("Float32")
			| Type::Basic("Int64") | Type::Basic("UInt64") | Type::Basic("Float64")
			| Type::Basic("IntPtr") | Type::Basic("UIntPtr")
				=> { lit.typ = expected_ty.clone(); expected_ty.clone() },
			_ => lit.pos.error(format!(
				"Type mismatch. Expected `{}`, found numeric literal",
				expected_ty)),
		}
	}

	fn infer_str_lit(&mut self, lit: &mut StrLit<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		if expected_ty.infer_by(&TYPE_BYTE_SLICE).is_some() {
			lit.typ = TYPE_BYTE_SLICE.clone();
			TYPE_BYTE_SLICE.clone()
		} else {
			lit.pos.error(TypeMis(expected_ty, &TYPE_BYTE_SLICE))
		}
	}

	fn infer_bool(&mut self, b: &mut Bool<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		if expected_ty.infer_by(&TYPE_BOOL).is_some() {
			b.typ = TYPE_BOOL.clone();
			TYPE_BOOL.clone()
		} else {
			b.pos.error(TypeMis(expected_ty, &TYPE_BOOL))
		}
	}

	fn infer_binding(&mut self, bnd: &mut Binding<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		let ident = bnd.path.as_ident().unwrap_or_else(|| unimplemented!());

		// In order to not violate any borrowing rules, use get_height to check if entry exists
		// and to speed up upcoming lookup
		if let Some(height) = self.extern_funcs.get_height(ident) {
			// Don't infer types for external items,
			// just check compatibility with expected_ty

			let extern_typ = self.extern_funcs.get_at_height(ident, height).unwrap();
			if let Some(inferred) = extern_typ.infer_by(expected_ty) {
				bnd.typ = inferred.clone();
				inferred
			} else {
				bnd.path.pos.error(TypeMis(expected_ty, &extern_typ))
			}
		} else if let Some(height) = self.const_defs.get_height(ident) {
			// Binding is a constant. Do inference

			let maybe_def = replace(
				self.const_defs.get_at_height_mut(ident, height).unwrap(),
				None);

			if let Some(mut def) = maybe_def {
				let old_vars = replace(&mut self.vars, Vec::new());

				let inferred = self.infer_const_def(&mut def, expected_ty);

				self.vars = old_vars;
				self.const_defs.update(ident, Some(def));
				bnd.typ = inferred.clone();

				inferred
			} else {
				// We are currently doing inference inside this definition, and as such
				// no more type information can be given for sure than Unknown

				Type::Unknown
			}
		} else if let Some(var_ty) = self.get_var_type_mut(ident) {
			// Binding is a variable

			if let Some(inferred) = var_ty.infer_by(expected_ty) {
				*var_ty = inferred.clone();
				bnd.typ = inferred.clone();
				inferred
			} else {
				bnd.path.pos.error(TypeMis(expected_ty, var_ty))
			}
		} else {
			bnd.path.pos.error(format!("Unresolved path `{}`", ident))
		}
	}

	fn infer_call_arg_types(&mut self, call: &mut Call<'src>)  {
		let proc_type = call.proced.get_type();

		let expected_types: Vec<Cow<Type>> = if proc_type.is_partially_known() {
			// The type of the procedure is not unknown.
			// If it's a valid procedure type, use it for inference

			match proc_type.get_proc_sig() {
				Some((param_tys, _)) if param_tys.len() == call.args.len() =>
					param_tys.iter().map(Cow::Borrowed).collect(),
				Some((param_tys, _)) => call.pos.error(
					format!("Arity mismatch. Expected {}, found {}",
						param_tys.len(),
						call.args.len())),
				None => call.proced.pos().error(
					TypeMis(
						&Type::new_proc(vec![Type::Unknown], Type::Unknown),
						&proc_type)),
			}
		} else {
			vec![call.proced.get_type(); call.args.len()]
		};

		for (arg, expect_ty) in call.args.iter_mut().zip(expected_types) {
			self.infer_expr(arg, &expect_ty);
		}
	}

	fn infer_call<'call>(&mut self, call: &'call mut Call<'src>, expected_ty: &Type<'src>)
		-> Cow<'call, Type<'src>>
	{
		self.infer_call_arg_types(call);

		let expected_proc_type = Type::new_proc(
			call.args.iter().map(|arg| arg.get_type().into_owned()).collect(),
			expected_ty.clone());

		let old_proc_typ = call.proced.get_type().into_owned();

		let proc_typ = self.infer_expr(&mut call.proced, &expected_proc_type);

		// TODO: This only works for function pointers, i.e. lambdas will need some different type.
		//       When traits are added, use a function trait like Rusts Fn/FnMut/FnOnce

		if old_proc_typ != proc_typ {
			// New type information regarding arg types available
			self.infer_call_arg_types(call);
		}

		call.get_type()
	}

	fn infer_block(&mut self, block: &mut Block<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		let (init, last) = if let Some((last, init)) = block.exprs.split_last_mut() {
			(init, last)
		} else {
			return TYPE_NIL.clone()
		};

		self.const_defs.push(
			replace(&mut block.const_defs, HashMap::new())
				.into_iter()
				.map(|(k, v)| (k, Some(v)))
				.collect());

		let old_vars_len = self.vars.len();

		// First pass. If possible, all vars defined in block should have types inferred.
		for expr in init.iter_mut() {
			if let Expr::VarDef(ref mut var_def) = *expr {
				self.infer_var_def(var_def, &Type::Unknown);
				self.vars.push((var_def.binding.clone(), var_def.body.get_type().into_owned()));
			} else {
				self.infer_expr(expr, &Type::Unknown);
			}
		}

		self.infer_expr(last, expected_ty);

		let mut block_defined_vars = self.vars.split_off(old_vars_len).into_iter();

		// Second pass. Infer types for all expressions in block now that types for all bindings
		// are, if possible, known.
		for expr in init {
			if let Expr::VarDef(ref mut var_def) = *expr {
				let v = block_defined_vars.next().expect("ICE: block_defined_vars empty");

				self.infer_expr(&mut var_def.body, &v.1);

				self.vars.push(v);
			} else {
				self.infer_expr(expr, &Type::Unknown);
			}
		}
		let last_typ = self.infer_expr(last, expected_ty);

		self.vars.truncate(old_vars_len);

		block.const_defs = self.const_defs.pop()
			.expect("ICE: ScopeStack was empty when replacing Block const defs")
			.into_iter()
			.map(|(k, v)| (k, v.expect("ICE: None when unmapping block const def")))
			.collect();

		last_typ
	}

	fn infer_if(&mut self, cond: &mut If<'src>, expected_typ: &Type<'src>) -> Type<'src> {
		self.infer_expr(&mut cond.predicate, &TYPE_BOOL);

		let cons_typ = self.infer_expr(&mut cond.consequent, expected_typ);
		let alt_typ = self.infer_expr(&mut cond.alternative, expected_typ);

		if let Some(inferred) = cons_typ.infer_by(&alt_typ) {
			if cons_typ == inferred && alt_typ == inferred {
				inferred
			} else {
				self.infer_if(cond, &inferred)
			}
		} else {
			cond.pos.error(ArmsDiffer(&cons_typ, &alt_typ))
		}
	}

	fn infer_lambda_args(&mut self, lam: &mut Lambda<'src>, expected_params: &[&Type<'src>]) {
		for (param, expected_param) in lam.params.iter_mut().zip(expected_params) {
			match param.typ.infer_by(expected_param) {
				Some(inferred) => param.typ = inferred,
				None => param.ident.pos.error(TypeMis(expected_param, &param.typ))
			}
		}
	}

	fn infer_lambda(&mut self, lam: &mut Lambda<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		let (expected_params, expected_body) = match expected_ty.get_proc_sig() {
			Some((params, _)) if params.len() != lam.params.len() =>
				lam.pos.error(TypeMis(expected_ty, &lam.get_type())),
			Some((params, body)) => (params.iter().collect(), body),
			None if *expected_ty == Type::Unknown => (
				vec![expected_ty; lam.params.len()],
				expected_ty,
			),
			None => lam.pos.error(TypeMis(expected_ty, &lam.get_type())),
		};

		// Own type is `Unknown` if no type has been inferred yet, or none was inferable

		let lam_typ = lam.get_type();

		if lam_typ.is_partially_known() {
			if let Some(inferred) = expected_ty.infer_by(&lam_typ) {
				if lam_typ == inferred {
					// Own type can't be inferred further by `expected_ty`
					return lam_typ;
				}
			} else {
				// Own type and expected type are not compatible. Type mismatch
				lam.pos.error(TypeMis(expected_ty, &lam_typ));
			}
		}

		self.infer_lambda_args(lam, &expected_params);

		let (vars_len, n_params) = (self.vars.len(), lam.params.len());

		self.vars.extend(lam.params.iter().cloned().map(|param| (param.ident, param.typ)));

		self.infer_expr(&mut lam.body, &expected_body);

		assert_eq!(self.vars.len(), vars_len + n_params);

		for (param, found) in lam.params.iter_mut()
			.zip(self.vars.drain(vars_len..))
			.map(|(param, (_, found))| (&mut param.typ, found))
		{
			*param = found;
		}

		lam.get_type()
	}

	fn infer_var_def(&mut self, def: &mut VarDef<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		if let Some(inferred) = expected_ty.infer_by(&TYPE_NIL) {
			self.infer_expr(&mut def.body, &Type::Unknown);
			def.typ = inferred.clone();
			inferred
		} else {
			def.pos.error(TypeMis(expected_ty, &TYPE_NIL))
		}
	}

	fn infer_assign(&mut self, assign: &mut Assign<'src>, expected_ty: &Type<'src>)
		-> Type<'src>
	{
		if let Some(inferred) = expected_ty.infer_by(&TYPE_NIL) {
			let rhs_typ = self.infer_expr(&mut assign.rhs, &assign.lhs.get_type());
			self.infer_expr(&mut assign.lhs, &rhs_typ);
			inferred
		} else {
			assign.pos.error(TypeMis(expected_ty, &TYPE_NIL))
		}
	}

	fn infer_symbol(&mut self, symbol: &mut Symbol<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		if let Some(inferred) = expected_ty.infer_by(&TYPE_SYMBOL) {
			symbol.typ = inferred.clone();
			inferred
		} else {
			symbol.ident.pos.error(TypeMis(expected_ty, &TYPE_SYMBOL))
		}
	}


	fn infer_deref(&mut self, deref: &mut Deref<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		let expected_ref_typ = Type::Construct("RawPtr", vec![expected_ty.clone()]);

		let ref_typ = self.infer_expr(&mut deref.r, &expected_ref_typ);

		match ref_typ {
			Type::Construct("RawPtr", mut args) => args.pop().unwrap_or_else(|| unreachable!()),
			_ => unreachable!(),
		}
	}

	fn infer_transmute<'ast>(&mut self, trans: &'ast mut Transmute<'src>, expected_ty: &Type<'src>)
		-> &'ast Type<'src>
	{
		if let Some(inferred) = trans.typ.infer_by(expected_ty) {
			trans.typ = inferred;

			self.infer_expr(&mut trans.arg, &Type::Unknown);

			&trans.typ
		} else {
			trans.pos.error(TypeMis(expected_ty, &trans.typ))
		}
	}

	fn infer_type_ascript(&mut self, expr: &mut Expr<'src>, expected_ty: &Type<'src>)
		-> Type<'src>
	{
		let (expected_ty2, inner_expr) = if let Expr::TypeAscript(ref mut ascr) = *expr {
			let expected_ty2 = expected_ty.infer_by(&ascr.typ)
				.unwrap_or_else(|| ascr.pos.error(TypeMis(expected_ty, &ascr.typ)));

			(expected_ty2, &mut ascr.expr as *mut _)
		} else {
			// This method should only be called when `expr` is a type ascription
			unreachable!()
		};

		// FIXME: Safe way of conducting this replacement
		unsafe { swap(expr, &mut *inner_expr) };

		self.infer_expr(expr, &expected_ty2)
	}

	fn infer_expr(&mut self, expr: &mut Expr<'src>, expected_ty: &Type<'src>) -> Type<'src> {
		let mut expected_ty = Cow::Borrowed(expected_ty);

		{
			let expr_typ = expr.get_type();

			// Own type is `Unknown` if no type has been inferred yet, or none was inferable
			if expr_typ.is_partially_known() {
				if let Some(inferred) = expected_ty.infer_by(&expr_typ) {
					if *expr_typ == inferred {
						// Own type can't be inferred further by `expected_ty`
						return expr_typ.into_owned();
					}
					expected_ty = Cow::Owned(inferred)
				} else {
					// Own type and expected type are not compatible. Type mismatch
					expr.pos().error(TypeMis(&expected_ty, &expr_typ));
				}
			}
		}

		// Own type is unknown, or `expected_ty` is more known than own type. Do inference

		match *expr {
			Expr::Nil(ref mut nil) => self.infer_nil(nil, &expected_ty),
			Expr::VarDef(ref mut def) => self.infer_var_def(def, &expected_ty),
			Expr::Assign(ref mut assign) => self.infer_assign(assign, &expected_ty),
			Expr::NumLit(ref mut l) => self.infer_num_lit(l, &expected_ty),
			Expr::StrLit(ref mut l) => self.infer_str_lit(l, &expected_ty),
			Expr::Bool(ref mut b) => self.infer_bool(b, &expected_ty),
			Expr::Binding(ref mut bnd) => self.infer_binding(bnd, &expected_ty),
			Expr::Call(ref mut call) => self.infer_call(call, &expected_ty).into_owned(),
			Expr::Block(ref mut block) => self.infer_block(block, &expected_ty),
			Expr::If(ref mut cond) => self.infer_if(cond, &expected_ty),
			Expr::Lambda(ref mut lam) => self.infer_lambda(lam, &expected_ty),
			Expr::Symbol(ref mut sym) => self.infer_symbol(sym, &expected_ty),
			Expr::Deref(ref mut deref) => self.infer_deref(deref, &expected_ty),
			Expr::Transmute(ref mut trans) => self.infer_transmute(trans, &expected_ty).clone(),
			Expr::TypeAscript(_) => self.infer_type_ascript(expr, &expected_ty),
		}
	}
}

pub fn infer_types(ast: &mut AST) {
	let mut inferer = Inferer::new(ast);

	let mut main = replace(
			inferer.const_defs.get_mut("main").expect("ICE: In infer_ast: No main def"),
			None)
		.unwrap();

	inferer.infer_const_def(
		&mut main,
		&Type::new_proc(vec![], Type::Basic("Int64")));

	inferer.const_defs.update("main", Some(main));

	let (const_defs, extern_funcs) = inferer.into_inner();

	ast.const_defs = const_defs;
	ast.extern_funcs = extern_funcs;
}
