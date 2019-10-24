use crate::ast;
use crate::error;
use crate::error::Error;
use crate::hir::*;
use crate::hir::index::Index;
use crate::hir::hir_maker_context::HirMakerContext;
use crate::type_checking;
use crate::parser::token::Token;

#[derive(Debug, PartialEq)]
pub struct HirMaker<'a> {
    pub index: &'a Index,
    // List of constants found so far
    pub constants: HashMap<ConstFullname, TermTy>,
    pub const_inits: Vec<HirExpression>,
}

impl<'a> HirMaker<'a> {
    fn new(index: &'a crate::hir::index::Index) -> HirMaker<'a> {
        HirMaker {
            index: index,
            constants: HashMap::new(),
            const_inits: vec![],
        }
    }

    pub fn convert_program(index: index::Index, prog: ast::Program) -> Result<Hir, Error> {
        let mut hir_maker = HirMaker::new(&index);

        let sk_methods =
            hir_maker.convert_toplevel_defs(&prog.toplevel_defs)?;
        let main_exprs =
            hir_maker.convert_exprs(&HirMakerContext::toplevel(), &prog.exprs)?;
        Ok(Hir {
            sk_classes: index.sk_classes,
            sk_methods,
            main_exprs,
        })
    }

    fn convert_toplevel_defs(&mut self, toplevel_defs: &Vec<ast::Definition>)
                            -> Result<HashMap<ClassFullname, Vec<SkMethod>>, Error> {
        let mut sk_methods = HashMap::new();
        let ctx = HirMakerContext::toplevel();

        toplevel_defs.iter().try_for_each(|def|
            match def {
                // Extract instance/class methods
                ast::Definition::ClassDefinition { name, defs } => {
                    match self.convert_class_def(&name, &defs) {
                        Ok((fullname, instance_methods, meta_name, class_methods)) => {
                            sk_methods.insert(fullname, instance_methods);
                            sk_methods.insert(meta_name, class_methods);
                            Ok(())
                        },
                        Err(err) => Err(err)
                    }
                },
                ast::Definition::ConstDefinition { name, expr } => {
                    self.register_const(&ctx, name, expr)
                }
                _ => panic!("should be checked in hir::index")
            }
        )?;

        Ok(sk_methods)
    }

    /// Extract instance/class methods and constants
    fn convert_class_def(&mut self, name: &ClassFirstname, defs: &Vec<ast::Definition>)
                        -> Result<(ClassFullname, Vec<SkMethod>,
                                   ClassFullname, Vec<SkMethod>), Error> {
        // TODO: nested class
        let fullname = name.to_class_fullname();
        let instance_ty = ty::raw(&fullname.0);
        let class_ty = instance_ty.meta_ty();
        let meta_name = class_ty.fullname.clone();

        let mut instance_methods = vec![];
        let mut class_methods = vec![];
        let ctx = HirMakerContext::class_ctx(&fullname);

        defs.iter().try_for_each(|def| {
            match def {
                ast::Definition::InstanceMethodDefinition { sig, body_exprs, .. } => {
                    match self.convert_method_def(&ctx, &fullname, &sig.name, &body_exprs) {
                        Ok(method) => { instance_methods.push(method); Ok(()) },
                        Err(err) => Err(err)
                    }
                },
                ast::Definition::ClassMethodDefinition { sig, body_exprs, .. } => {
                    match self.convert_method_def(&ctx, &meta_name, &sig.name, &body_exprs) {
                        Ok(method) => { class_methods.push(method); Ok(()) },
                        Err(err) => Err(err)
                    }
                },
                ast::Definition::ConstDefinition { name, expr } => {
                    self.register_const(&ctx, name, expr)
                }
                _ => Ok(()),
            }
        })?;

        class_methods.push(self.create_new(&fullname));
        Ok((fullname, instance_methods, meta_name, class_methods))
    }

    /// Create .new
    fn create_new(&self, fullname: &ClassFullname) -> SkMethod {
        let class_fullname = fullname.clone();
        let instance_ty = ty::raw(&class_fullname.0);
        let class_ty = instance_ty.meta_ty();
        let meta_name = class_ty.fullname;

        SkMethod {
            signature: signature_of_new(&meta_name, &instance_ty),
            body: SkMethodBody::RustClosureMethodBody {
                boxed_gen: Box::new(move |code_gen, _| {
                    let addr = code_gen.allocate_sk_obj(&class_fullname);
                    code_gen.builder.build_return(Some(&addr));
                    Ok(())
                })
            }
        }
    }

    /// Register a constant
    fn register_const(&mut self,
                      ctx: &HirMakerContext,
                      name: &ConstFirstname,
                      expr: &ast::Expression) -> Result<(), Error> {
        let fullname = ConstFullname(ctx.namespace.0.clone() + "::" + &name.0);
        let hir_expr = self.convert_expr(ctx, expr)?;
        self.constants.insert(fullname.clone(), hir_expr.ty.clone());
        let op = Hir::assign_const(fullname, hir_expr);
        self.const_inits.push(op);
        Ok(())
    }


    fn convert_method_def(&self,
                          ctx: &HirMakerContext,
                          class_fullname: &ClassFullname,
                          name: &MethodFirstname,
                          body_exprs: &Vec<ast::Expression>) -> Result<SkMethod, Error> {
        // MethodSignature is built beforehand by index::new
        let err = format!("[BUG] signature not found ({}/{}/{:?})", class_fullname, name, self.index);
        let signature = self.index.find_method(class_fullname, name).expect(&err).clone();

        let method_ctx = HirMakerContext::method_ctx(ctx, &signature);
        let body_exprs = self.convert_exprs(&method_ctx, body_exprs)?;
        type_checking::check_return_value(&signature, &body_exprs.ty)?;

        let body = SkMethodBody::ShiikaMethodBody { exprs: body_exprs };

        Ok(SkMethod { signature, body })
    }

    fn convert_exprs(&self,
                     ctx: &HirMakerContext,
                     exprs: &Vec<ast::Expression>) -> Result<HirExpressions, Error> {
        let hir_exprs = exprs.iter().map(|expr|
            self.convert_expr(ctx, expr)
        ).collect::<Result<Vec<_>, _>>()?;

        let ty = match hir_exprs.last() {
                   Some(hir_expr) => hir_expr.ty.clone(),
                   None => ty::raw("Void"),
                 };

        Ok(HirExpressions { ty: ty, exprs: hir_exprs })
    }

    fn convert_expr(&self,
                    ctx: &HirMakerContext,
                    expr: &ast::Expression) -> Result<HirExpression, Error> {
        match &expr.body {
            ast::ExpressionBody::If { cond_expr, then_expr, else_expr } => {
                let cond_hir = self.convert_expr(ctx, cond_expr)?;
                type_checking::check_if_condition_ty(&cond_hir.ty)?;

                let then_hir = self.convert_expr(ctx, then_expr)?;
                let else_hir = match else_expr {
                    Some(expr) => self.convert_expr(ctx, expr)?,
                    None => Hir::nop(),
                };
                // TODO: then and else must have conpatible type
                Ok(Hir::if_expression(
                        then_hir.ty.clone(),
                        cond_hir,
                        then_hir,
                        else_hir))
            },

            ast::ExpressionBody::ConstAssign { names, rhs } => {
                // TODO: resolve name
                let fullname = ConstFullname(names.join("::"));
                Ok(Hir::assign_const(fullname, self.convert_expr(ctx, rhs)?))
            },

            ast::ExpressionBody::MethodCall {receiver_expr, method_name, arg_exprs, .. } => {
                let receiver_hir =
                    match receiver_expr {
                        Some(expr) => self.convert_expr(ctx, &expr)?,
                        // Implicit self
                        _ => self.convert_self_expr(ctx)?,
                    };
                // TODO: arg types must match with method signature
                let arg_hirs = arg_exprs.iter().map(|arg_expr| self.convert_expr(ctx, arg_expr)).collect::<Result<Vec<_>,_>>()?;

                self.make_method_call(receiver_hir, &method_name, arg_hirs)
            },

            ast::ExpressionBody::BareName(name) => {
                self.convert_bare_name(ctx, name)
            },

            ast::ExpressionBody::ConstRef(names) => {
                self.convert_const_ref(ctx, names)
            },

            ast::ExpressionBody::PseudoVariable(token) => {
                self.convert_pseudo_variable(ctx, token)
            },

            ast::ExpressionBody::FloatLiteral {value} => {
                Ok(Hir::float_literal(*value))
            },

            ast::ExpressionBody::DecimalLiteral {value} => {
                Ok(Hir::decimal_literal(*value))
            },

            x => panic!("TODO: {:?}", x)
        }
    }

    fn convert_pseudo_variable(&self,
                               ctx: &HirMakerContext,
                               token: &Token) -> Result<HirExpression, Error> {
        match token {
            Token::KwSelf => {
                self.convert_self_expr(ctx)
            },
            Token::KwTrue => {
                Ok(Hir::boolean_literal(true))
            },
            Token::KwFalse => {
                Ok(Hir::boolean_literal(false))
            },
            _ => panic!("[BUG] not a pseudo variable token: {:?}", token)
        }
    }

    fn convert_self_expr(&self, ctx: &HirMakerContext) -> Result<HirExpression, Error> {
        Ok(Hir::self_expression(ctx.self_ty.clone()))
    }

    /// Generate local variable reference or method call with implicit receiver(self)
    fn convert_bare_name(&self,
                         ctx: &HirMakerContext,
                         name: &str) -> Result<HirExpression, Error> {
        let method_sig = match &ctx.method_sig {
            Some(x) => x,
            None => return Err(error::program_error(&format!("bare name outside method: `{}'", name)))
        };
        match &method_sig.find_param(name) {
            Some((idx, param)) => {
                Ok(Hir::hir_arg_ref(param.ty.clone(), *idx))
            },
            None => {
                Err(error::program_error(&format!("variable `{}' was not found", name)))
            }
        }
    }

    /// Resolve constant name
    fn convert_const_ref(&self,
                         _ctx: &HirMakerContext,
                         names: &Vec<String>) -> Result<HirExpression, Error> {
        // TODO: Resolve using ctx
        let name = &names[0];
        if self.index.class_exists(&name) {
            let ty = ty::meta(name);
            Ok(Hir::const_ref(ty, ConstFullname(name.to_string())))
        }
        else {
            Err(error::program_error(&format!("constant `{}' was not found", name)))
        }
    }

    fn make_method_call(&self, receiver_hir: HirExpression, method_name: &MethodFirstname, arg_hirs: Vec<HirExpression>) -> Result<HirExpression, Error> {
        let class_fullname = &receiver_hir.ty.fullname;
        let sig = self.lookup_method(class_fullname, class_fullname, method_name)?;

        let param_tys = arg_hirs.iter().map(|expr| &expr.ty).collect();
        type_checking::check_method_args(&sig, &param_tys)?;

        Ok(Hir::method_call(sig.ret_ty.clone(), receiver_hir, sig.fullname.clone(), arg_hirs))
    }

    fn lookup_method(&self, 
                     receiver_class_fullname: &ClassFullname,
                     class_fullname: &ClassFullname,
                     method_name: &MethodFirstname) -> Result<&MethodSignature, Error> {
        let found = self.index.find_method(class_fullname, method_name);
        if let Some(sig) = found {
            Ok(sig)
        }
        else {
            // Look up in superclass
            let sk_class = self.index.find_class(class_fullname)
                .expect("[BUG] lookup_method: class not found");
            if let Some(super_name) = &sk_class.superclass_fullname {
                self.lookup_method(receiver_class_fullname, super_name, method_name)
            }
            else {
                Err(error::program_error(&format!("method {:?} not found on {:?}", method_name, receiver_class_fullname)))
            }
        }
    }
}
