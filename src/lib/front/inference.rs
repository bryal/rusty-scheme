//! Type inference

// TODO: Almost all `infer_types` takes const map + var stack + caller stack.
//       Maybe encapsulate this using some kind of state
// TODO: Replace verbose type error checks with:
//       `foo.infer_types(...)
//           .unwrap_or_else(foo.pos().type_mismatcher(expected_ty))`

use self::InferenceErr::*;
use lib::collections::ScopeStack;
use lib::front::ast::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::mem::{replace, swap};

enum InferenceErr<'p, 'src: 'p> {
    /// Type mismatch. (expected, found)
    TypeMis(&'p Type<'src>, &'p Type<'src>),
    ArmsDiffer(&'p Type<'src>, &'p Type<'src>),
    NonNilNullary(&'p Type<'src>),
}
impl<'src, 'p> Display for InferenceErr<'src, 'p> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            TypeMis(expected, found) => {
                write!(f,
                       "Type mismatch. Expected `{}`, found `{}`",
                       expected,
                       found)
            }
            ArmsDiffer(c, a) => {
                write!(f,
                       "Consequent and alternative have different types. Expected `{}` from \
                        alternative, found `{}`",
                       c,
                       a)
            }
            NonNilNullary(t) => write!(f,
                                       "Infering non-nil type `{}` for the parameter of a \
                                        nullary function",
                                       t),
        }
    }
}

struct Inferer<'src> {
    vars: Vec<(&'src str, Type<'src>)>,
    static_defs: ScopeStack<&'src str, Option<StaticDef<'src>>>,
    extern_funcs: ScopeStack<&'src str, ExternProcDecl<'src>>,
}
impl<'src> Inferer<'src> {
    fn new(ast: &mut Module<'src>) -> Self {
        let mut static_defs = ScopeStack::new();
        static_defs.push(replace(&mut ast.static_defs, HashMap::new())
            .into_iter()
            .map(|(k, v)| (k, Some(v)))
            .collect());

        let mut extern_funcs = ScopeStack::new();
        extern_funcs.push(replace(&mut ast.extern_funcs, HashMap::new()));

        Inferer {
            vars: Vec::new(),
            static_defs: static_defs,
            extern_funcs: extern_funcs,
        }
    }

    fn into_inner
        (mut self)
         -> (HashMap<&'src str, StaticDef<'src>>, HashMap<&'src str, ExternProcDecl<'src>>) {
        let static_defs =
            self.static_defs
                .pop()
                .expect("ICE: Inferer::into_inner: static_defs.pop() failed")
                .into_iter()
                .map(|(k, v)| {
                    (k, v.expect("ICE: Inferer::into_inner: None when unmapping const def"))
                })
                .collect();
        let extern_funcs = self.extern_funcs
                               .pop()
                               .expect("ICE: Inferer::into_inner: extern_funcs.pop() failed");

        (static_defs, extern_funcs)
    }

    fn get_var_type_mut(&mut self, id: &str) -> Option<&mut Type<'src>> {
        self.vars.iter_mut().rev().find(|&&mut (b, _)| b == id).map(|&mut (_, ref mut t)| t)
    }

    fn infer_static_def(&mut self,
                        def: &mut StaticDef<'src>,
                        expected_ty: &Type<'src>)
                        -> Type<'src> {
        self.infer_expr(&mut def.body, expected_ty)
    }

    fn infer_nil(&mut self, nil: &mut Nil<'src>, expected_ty: &Type<'src>) -> Type<'src> {
        if let Some(type_nil) = expected_ty.infer_by(&TYPE_NIL) {
            type_nil
        } else {
            nil.pos.error_exit(TypeMis(expected_ty, &TYPE_NIL))
        }
    }

    fn infer_num_lit(&mut self, lit: &mut NumLit<'src>, expected_ty: &Type<'src>) -> Type<'src> {
        match *expected_ty {
            Type::Uninferred |
            Type::Const("Int8") |
            Type::Const("UInt8") |
            Type::Const("Int16") |
            Type::Const("UInt16") |
            Type::Const("Int32") |
            Type::Const("UInt32") |
            Type::Const("Float32") |
            Type::Const("Int64") |
            Type::Const("UInt64") |
            Type::Const("Float64") |
            Type::Const("IntPtr") |
            Type::Const("UIntPtr") => {
                lit.typ = expected_ty.clone();
                expected_ty.clone()
            }
            _ => {
                lit.pos.error_exit(format!("Type mismatch. Expected `{}`, found numeric literal",
                                           expected_ty))
            }
        }
    }

    fn infer_str_lit(&mut self, lit: &mut StrLit<'src>, expected_ty: &Type<'src>) -> Type<'src> {
        if expected_ty.infer_by(&TYPE_BYTE_SLICE).is_some() {
            lit.typ = TYPE_BYTE_SLICE.clone();
            TYPE_BYTE_SLICE.clone()
        } else {
            lit.pos.error_exit(TypeMis(expected_ty, &TYPE_BYTE_SLICE))
        }
    }

    fn infer_bool(&mut self, b: &mut Bool<'src>, expected_ty: &Type<'src>) -> Type<'src> {
        if expected_ty.infer_by(&TYPE_BOOL).is_some() {
            TYPE_BOOL.clone()
        } else {
            b.pos.error_exit(TypeMis(expected_ty, &TYPE_BOOL))
        }
    }

    fn infer_binding(&mut self, bnd: &mut Binding<'src>, expected_ty: &Type<'src>) -> Type<'src> {
        // In order to not violate any borrowing rules, use get_height to check if entry exists
        // and to speed up upcoming lookup
        if let Some(height) = self.extern_funcs.get_height(bnd.ident.s) {
            // Don't infer types for external items,
            // just check compatibility with expected_ty

            let extern_typ = &self.extern_funcs.get_at_height(bnd.ident.s, height).unwrap().typ;
            if let Some(inferred) = extern_typ.infer_by(expected_ty) {
                bnd.typ = inferred.clone();
                inferred
            } else {
                bnd.ident.pos.error_exit(TypeMis(expected_ty, &extern_typ))
            }
        } else if let Some(height) = self.static_defs.get_height(bnd.ident.s) {
            // Binding is a constant. Do inference

            let maybe_def =
                replace(self.static_defs.get_at_height_mut(bnd.ident.s, height).unwrap(),
                        None);

            if let Some(mut def) = maybe_def {
                let old_vars = replace(&mut self.vars, Vec::new());

                let inferred = self.infer_static_def(&mut def, expected_ty);

                self.vars = old_vars;
                self.static_defs.update(bnd.ident.s, Some(def));
                bnd.typ = inferred.clone();

                inferred
            } else {
                // We are currently doing inference inside this definition, and as such
                // no more type information can be given for sure than Unknown

                Type::Uninferred
            }
        } else if let Some(var_ty) = self.get_var_type_mut(bnd.ident.s) {
            // Binding is a variable

            if let Some(inferred) = var_ty.infer_by(expected_ty) {
                *var_ty = inferred.clone();
                bnd.typ = inferred.clone();
                inferred
            } else {
                bnd.ident.pos.error_exit(TypeMis(expected_ty, var_ty))
            }
        } else {
            bnd.ident.pos.error_exit(format!("Unresolved path `{}`", bnd.ident))
        }
    }

    fn infer_call_arg(&mut self, call: &mut Call<'src>) {
        let func_type = call.func.get_type();

        let expected_typ: &Type = if func_type.is_partially_known() {
            &func_type.get_func_sig().unwrap_or_else(|| unreachable!()).0
        } else {
            &TYPE_UNINFERRED
        };

        if let Some(ref mut arg) = call.arg {
            self.infer_expr(arg, expected_typ);
        }
    }

    fn infer_call<'call>(&mut self,
                         call: &'call mut Call<'src>,
                         expected_ty: &Type<'src>)
                         -> &'call Type<'src> {
        self.infer_call_arg(call);

        let arg_typ = call.arg.as_ref().map(|arg| arg.get_type()).unwrap_or(&TYPE_NIL).clone();
        let expected_func_type = Type::new_func(arg_typ, expected_ty.clone());

        let old_func_typ = call.func.get_type().clone();

        let func_typ = self.infer_expr(&mut call.func, &expected_func_type);

        // TODO: This only works for function pointers, i.e. lambdas will need some different type.
        //       When traits are added, use a function trait like Rusts Fn/FnMut/FnOnce

        if old_func_typ != func_typ {
            // New type information regarding arg types available
            self.infer_call_arg(call);
        }

        call.typ = call.func
                       .get_type()
                       .get_func_sig()
                       .map(|(_, ret_typ)| ret_typ.clone())
                       .unwrap_or(Type::Uninferred);

        &call.typ
    }

    fn infer_block<'a>(&mut self,
                       block: &'a mut Block<'src>,
                       expected_ty: &Type<'src>)
                       -> &'a Type<'src> {
        let (init, last) = if let Some((last, init)) = block.exprs.split_last_mut() {
            (init, last)
        } else {
            return &TYPE_NIL;
        };

        self.static_defs.push(replace(&mut block.static_defs, HashMap::new())
            .into_iter()
            .map(|(k, v)| (k, Some(v)))
            .collect());

        for expr in init.iter_mut() {
            self.infer_expr(expr, &Type::Uninferred);
        }

        let last_typ = self.infer_expr(last, expected_ty);

        block.static_defs =
            self.static_defs
                .pop()
                .expect("ICE: ScopeStack was empty when replacing Block const defs")
                .into_iter()
                .map(|(k, v)| (k, v.expect("ICE: None when unmapping block const def")))
                .collect();

        block.typ = last_typ;
        &block.typ
    }

    fn infer_if(&mut self, cond: &mut If<'src>, expected_typ: &Type<'src>) -> Type<'src> {
        self.infer_expr(&mut cond.predicate, &TYPE_BOOL);

        let cons_typ = self.infer_expr(&mut cond.consequent, expected_typ);
        let alt_typ = self.infer_expr(&mut cond.alternative, expected_typ);

        if let Some(inferred) = cons_typ.infer_by(&alt_typ) {
            let cons_typ = self.infer_expr(&mut cond.consequent, &inferred);
            let alt_typ = self.infer_expr(&mut cond.alternative, &inferred);

            if cons_typ == inferred && alt_typ == inferred { inferred } else { Type::Uninferred }
        } else {
            cond.pos.error_exit(ArmsDiffer(&cons_typ, &alt_typ))
        }
    }

    fn infer_param(&mut self, lam: &mut Lambda<'src>, expected_typ: &Type<'src>) {
        match lam.param {
            Some(ref mut param) => match param.typ.infer_by(expected_typ) {
                Some(inferred) => param.typ = inferred,
                None => param.ident.pos.error_exit(TypeMis(expected_typ, &param.typ)),
            },
            None => if expected_typ.infer_by(&TYPE_NIL).is_none() {
                lam.pos.error_exit(NonNilNullary(expected_typ))
            },
        }
    }

    fn infer_lambda<'l>(&mut self,
                        mut lam: &'l mut Lambda<'src>,
                        expected_ty: &Type<'src>)
                        -> &'l Type<'src> {
        let (expected_param, expected_body) = expected_ty.get_func_sig()
                                                         .unwrap_or((&TYPE_UNINFERRED,
                                                                     &TYPE_UNINFERRED));

        // Own type is `Unknown` if no type has been inferred yet, or none was inferable

        if lam.typ.is_partially_known() {
            if let Some(inferred) = expected_ty.infer_by(&lam.typ) {
                if lam.typ == inferred {
                    // Own type can't be inferred further by `expected_ty`
                    return &lam.typ;
                }
            } else {
                // Own type and expected type are not compatible. Type mismatch
                lam.pos.error_exit(TypeMis(expected_ty, &lam.typ));
            }
        }

        self.infer_param(&mut lam, &expected_param);

        if let Some(ref mut param) = lam.param {
            self.vars.push((param.ident.s, param.typ.clone()));
            self.infer_expr(&mut lam.body, &expected_body);
            param.typ = self.vars
                            .pop()
                            .expect("ICE: Variable stack empty after infer expr when infer lambda")
                            .1;
        } else {
            self.infer_expr(&mut lam.body, &expected_body);
        }

        lam.typ = Type::new_func(get_param_type(&lam.param).clone(),
                                 lam.body.get_type().clone());
        &lam.typ
    }

    fn infer_assign(&mut self, assign: &mut Assign<'src>, expected_ty: &Type<'src>) -> Type<'src> {
        if let Some(inferred) = expected_ty.infer_by(&TYPE_NIL) {
            let rhs_typ = self.infer_expr(&mut assign.rhs, &assign.lhs.get_type());
            self.infer_expr(&mut assign.lhs, &rhs_typ);
            assign.typ = inferred.clone();
            inferred
        } else {
            assign.pos.error_exit(TypeMis(expected_ty, &TYPE_NIL))
        }
    }

    fn infer_type_ascript(&mut self,
                          expr: &mut Expr<'src>,
                          expected_ty: &Type<'src>)
                          -> Type<'src> {
        let (expected_ty2, inner_expr) = if let Expr::TypeAscript(ref mut ascr) = *expr {
            let expected_ty2 =
                expected_ty.infer_by(&ascr.typ)
                           .unwrap_or_else(|| ascr.pos.error_exit(TypeMis(expected_ty, &ascr.typ)));

            (expected_ty2, &mut ascr.expr as *mut _)
        } else {
            // This method should only be called when `expr` is a type ascription
            unreachable!()
        };

        // FIXME: Safe way of conducting this replacement
        unsafe { swap(expr, &mut *inner_expr) };

        self.infer_expr(expr, &expected_ty2)
    }

    fn infer_cons(&mut self, cons: &mut Cons<'src>, expected_ty: &Type<'src>) -> Type<'src> {
        let unknown_cons_typ = Type::new_cons(Type::Uninferred, Type::Uninferred);

        let maybe_expected_inferred = expected_ty.infer_by(&unknown_cons_typ);
        if let Some((e_car, e_cdr)) = maybe_expected_inferred.as_ref().and_then(|t| t.get_cons()) {
            let car_typ = self.infer_expr(&mut cons.car, e_car);
            let cdr_typ = self.infer_expr(&mut cons.cdr, e_cdr);
            cons.typ = Type::new_cons(car_typ, cdr_typ);
            cons.typ.clone()
        } else {
            cons.pos.error_exit(TypeMis(expected_ty, &unknown_cons_typ))
        }
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
                        return expr_typ.clone();
                    }
                    expected_ty = Cow::Owned(inferred)
                } else {
                    // Own type and expected type are not compatible. Type mismatch
                    expr.pos().error_exit(TypeMis(&expected_ty, &expr_typ));
                }
            }
        }

        // Own type is unknown, or `expected_ty` is more known than own type. Do inference

        match *expr {
            Expr::Nil(ref mut nil) => self.infer_nil(nil, &expected_ty),
            Expr::Assign(ref mut assign) => self.infer_assign(assign, &expected_ty),
            Expr::NumLit(ref mut l) => self.infer_num_lit(l, &expected_ty),
            Expr::StrLit(ref mut l) => self.infer_str_lit(l, &expected_ty),
            Expr::Bool(ref mut b) => self.infer_bool(b, &expected_ty),
            Expr::Binding(ref mut bnd) => self.infer_binding(bnd, &expected_ty),
            Expr::Call(ref mut call) => self.infer_call(call, &expected_ty).clone(),
            Expr::Block(ref mut block) => self.infer_block(block, &expected_ty).clone(),
            Expr::If(ref mut cond) => self.infer_if(cond, &expected_ty),
            Expr::Lambda(ref mut lam) => self.infer_lambda(lam, &expected_ty).clone(),
            Expr::TypeAscript(_) => self.infer_type_ascript(expr, &expected_ty),
            Expr::Cons(ref mut cons) => self.infer_cons(cons, &expected_ty),
        }
    }
}

pub fn infer_types(ast: &mut Module) {
    let mut inferer = Inferer::new(ast);

    let mut main = replace(inferer.static_defs
                                  .get_mut("main")
                                  .expect("ICE: In infer_types: No main def"),
                           None)
                       .unwrap();

    inferer.infer_static_def(&mut main,
                             &Type::new_func(TYPE_NIL.clone(), Type::Const("Int64")));

    inferer.static_defs.update("main", Some(main));

    let (static_defs, extern_funcs) = inferer.into_inner();

    ast.static_defs = static_defs;
    ast.extern_funcs = extern_funcs;
}
