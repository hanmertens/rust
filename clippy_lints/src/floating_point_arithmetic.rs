use crate::consts::{
    constant, constant_simple, Constant,
    Constant::{Int, F32, F64},
};
use crate::utils::{higher, numeric_literal, span_lint_and_sugg, sugg, SpanlessEq};
use if_chain::if_chain;
use rustc_errors::Applicability;
use rustc_hir::{BinOpKind, Expr, ExprKind, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty;
use rustc_session::{declare_lint_pass, declare_tool_lint};
use rustc_span::source_map::Spanned;

use rustc_ast::ast;
use std::f32::consts as f32_consts;
use std::f64::consts as f64_consts;
use sugg::Sugg;

declare_clippy_lint! {
    /// **What it does:** Looks for floating-point expressions that
    /// can be expressed using built-in methods to improve accuracy
    /// at the cost of performance.
    ///
    /// **Why is this bad?** Negatively impacts accuracy.
    ///
    /// **Known problems:** None
    ///
    /// **Example:**
    ///
    /// ```rust
    /// let a = 3f32;
    /// let _ = a.powf(1.0 / 3.0);
    /// let _ = (1.0 + a).ln();
    /// let _ = a.exp() - 1.0;
    /// ```
    ///
    /// is better expressed as
    ///
    /// ```rust
    /// let a = 3f32;
    /// let _ = a.cbrt();
    /// let _ = a.ln_1p();
    /// let _ = a.exp_m1();
    /// ```
    pub IMPRECISE_FLOPS,
    nursery,
    "usage of imprecise floating point operations"
}

declare_clippy_lint! {
    /// **What it does:** Looks for floating-point expressions that
    /// can be expressed using built-in methods to improve both
    /// accuracy and performance.
    ///
    /// **Why is this bad?** Negatively impacts accuracy and performance.
    ///
    /// **Known problems:** None
    ///
    /// **Example:**
    ///
    /// ```rust
    /// use std::f32::consts::E;
    ///
    /// let a = 3f32;
    /// let _ = (2f32).powf(a);
    /// let _ = E.powf(a);
    /// let _ = a.powf(1.0 / 2.0);
    /// let _ = a.log(2.0);
    /// let _ = a.log(10.0);
    /// let _ = a.log(E);
    /// let _ = a.powf(2.0);
    /// let _ = a * 2.0 + 4.0;
    /// let _ = if a < 0.0 {
    ///     -a
    /// } else {
    ///     a
    /// };
    /// let _ = if a < 0.0 {
    ///     a
    /// } else {
    ///     -a
    /// };
    /// ```
    ///
    /// is better expressed as
    ///
    /// ```rust
    /// use std::f32::consts::E;
    ///
    /// let a = 3f32;
    /// let _ = a.exp2();
    /// let _ = a.exp();
    /// let _ = a.sqrt();
    /// let _ = a.log2();
    /// let _ = a.log10();
    /// let _ = a.ln();
    /// let _ = a.powi(2);
    /// let _ = a.mul_add(2.0, 4.0);
    /// let _ = a.abs();
    /// let _ = -a.abs();
    /// ```
    pub SUBOPTIMAL_FLOPS,
    nursery,
    "usage of sub-optimal floating point operations"
}

declare_lint_pass!(FloatingPointArithmetic => [
    IMPRECISE_FLOPS,
    SUBOPTIMAL_FLOPS
]);

// Returns the specialized log method for a given base if base is constant
// and is one of 2, 10 and e
fn get_specialized_log_method(cx: &LateContext<'_>, base: &Expr<'_>) -> Option<&'static str> {
    if let Some((value, _)) = constant(cx, cx.tables(), base) {
        if F32(2.0) == value || F64(2.0) == value {
            return Some("log2");
        } else if F32(10.0) == value || F64(10.0) == value {
            return Some("log10");
        } else if F32(f32_consts::E) == value || F64(f64_consts::E) == value {
            return Some("ln");
        }
    }

    None
}

// Adds type suffixes and parenthesis to method receivers if necessary
fn prepare_receiver_sugg<'a>(cx: &LateContext<'_>, mut expr: &'a Expr<'a>) -> Sugg<'a> {
    let mut suggestion = Sugg::hir(cx, expr, "..");

    if let ExprKind::Unary(UnOp::UnNeg, inner_expr) = &expr.kind {
        expr = &inner_expr;
    }

    if_chain! {
        // if the expression is a float literal and it is unsuffixed then
        // add a suffix so the suggestion is valid and unambiguous
        if let ty::Float(float_ty) = cx.tables().expr_ty(expr).kind;
        if let ExprKind::Lit(lit) = &expr.kind;
        if let ast::LitKind::Float(sym, ast::LitFloatType::Unsuffixed) = lit.node;
        then {
            let op = format!(
                "{}{}{}",
                suggestion,
                // Check for float literals without numbers following the decimal
                // separator such as `2.` and adds a trailing zero
                if sym.as_str().ends_with('.') {
                    "0"
                } else {
                    ""
                },
                float_ty.name_str()
            ).into();

            suggestion = match suggestion {
                Sugg::MaybeParen(_) => Sugg::MaybeParen(op),
                _ => Sugg::NonParen(op)
            };
        }
    }

    suggestion.maybe_par()
}

fn check_log_base(cx: &LateContext<'_>, expr: &Expr<'_>, args: &[Expr<'_>]) {
    if let Some(method) = get_specialized_log_method(cx, &args[1]) {
        span_lint_and_sugg(
            cx,
            SUBOPTIMAL_FLOPS,
            expr.span,
            "logarithm for bases 2, 10 and e can be computed more accurately",
            "consider using",
            format!("{}.{}()", Sugg::hir(cx, &args[0], ".."), method),
            Applicability::MachineApplicable,
        );
    }
}

// TODO: Lint expressions of the form `(x + y).ln()` where y > 1 and
// suggest usage of `(x + (y - 1)).ln_1p()` instead
fn check_ln1p(cx: &LateContext<'_>, expr: &Expr<'_>, args: &[Expr<'_>]) {
    if let ExprKind::Binary(
        Spanned {
            node: BinOpKind::Add, ..
        },
        lhs,
        rhs,
    ) = &args[0].kind
    {
        let recv = match (constant(cx, cx.tables(), lhs), constant(cx, cx.tables(), rhs)) {
            (Some((value, _)), _) if F32(1.0) == value || F64(1.0) == value => rhs,
            (_, Some((value, _))) if F32(1.0) == value || F64(1.0) == value => lhs,
            _ => return,
        };

        span_lint_and_sugg(
            cx,
            IMPRECISE_FLOPS,
            expr.span,
            "ln(1 + x) can be computed more accurately",
            "consider using",
            format!("{}.ln_1p()", prepare_receiver_sugg(cx, recv)),
            Applicability::MachineApplicable,
        );
    }
}

// Returns an integer if the float constant is a whole number and it can be
// converted to an integer without loss of precision. For now we only check
// ranges [-16777215, 16777216) for type f32 as whole number floats outside
// this range are lossy and ambiguous.
#[allow(clippy::cast_possible_truncation)]
fn get_integer_from_float_constant(value: &Constant) -> Option<i32> {
    match value {
        F32(num) if num.fract() == 0.0 => {
            if (-16_777_215.0..16_777_216.0).contains(num) {
                Some(num.round() as i32)
            } else {
                None
            }
        },
        F64(num) if num.fract() == 0.0 => {
            if (-2_147_483_648.0..2_147_483_648.0).contains(num) {
                Some(num.round() as i32)
            } else {
                None
            }
        },
        _ => None,
    }
}

fn check_powf(cx: &LateContext<'_>, expr: &Expr<'_>, args: &[Expr<'_>]) {
    // Check receiver
    if let Some((value, _)) = constant(cx, cx.tables(), &args[0]) {
        let method = if F32(f32_consts::E) == value || F64(f64_consts::E) == value {
            "exp"
        } else if F32(2.0) == value || F64(2.0) == value {
            "exp2"
        } else {
            return;
        };

        span_lint_and_sugg(
            cx,
            SUBOPTIMAL_FLOPS,
            expr.span,
            "exponent for bases 2 and e can be computed more accurately",
            "consider using",
            format!("{}.{}()", prepare_receiver_sugg(cx, &args[1]), method),
            Applicability::MachineApplicable,
        );
    }

    // Check argument
    if let Some((value, _)) = constant(cx, cx.tables(), &args[1]) {
        let (lint, help, suggestion) = if F32(1.0 / 2.0) == value || F64(1.0 / 2.0) == value {
            (
                SUBOPTIMAL_FLOPS,
                "square-root of a number can be computed more efficiently and accurately",
                format!("{}.sqrt()", Sugg::hir(cx, &args[0], "..")),
            )
        } else if F32(1.0 / 3.0) == value || F64(1.0 / 3.0) == value {
            (
                IMPRECISE_FLOPS,
                "cube-root of a number can be computed more accurately",
                format!("{}.cbrt()", Sugg::hir(cx, &args[0], "..")),
            )
        } else if let Some(exponent) = get_integer_from_float_constant(&value) {
            (
                SUBOPTIMAL_FLOPS,
                "exponentiation with integer powers can be computed more efficiently",
                format!(
                    "{}.powi({})",
                    Sugg::hir(cx, &args[0], ".."),
                    numeric_literal::format(&exponent.to_string(), None, false)
                ),
            )
        } else {
            return;
        };

        span_lint_and_sugg(
            cx,
            lint,
            expr.span,
            help,
            "consider using",
            suggestion,
            Applicability::MachineApplicable,
        );
    }
}

fn check_powi(cx: &LateContext<'_, '_>, expr: &Expr<'_>, args: &[Expr<'_>]) {
    // Check argument
    if let Some((value, _)) = constant(cx, cx.tables, &args[1]) {
        let (lint, help, suggestion) = match value {
            Int(2) => (
                IMPRECISE_FLOPS,
                "square can be computed more accurately",
                format!("{} * {}", Sugg::hir(cx, &args[0], ".."), Sugg::hir(cx, &args[0], "..")),
            ),
            _ => return,
        };

        span_lint_and_sugg(
            cx,
            lint,
            expr.span,
            help,
            "consider using",
            suggestion,
            Applicability::MachineApplicable,
        );
    }
}

// TODO: Lint expressions of the form `x.exp() - y` where y > 1
// and suggest usage of `x.exp_m1() - (y - 1)` instead
fn check_expm1(cx: &LateContext<'_>, expr: &Expr<'_>) {
    if_chain! {
        if let ExprKind::Binary(Spanned { node: BinOpKind::Sub, .. }, ref lhs, ref rhs) = expr.kind;
        if cx.tables().expr_ty(lhs).is_floating_point();
        if let Some((value, _)) = constant(cx, cx.tables(), rhs);
        if F32(1.0) == value || F64(1.0) == value;
        if let ExprKind::MethodCall(ref path, _, ref method_args, _) = lhs.kind;
        if cx.tables().expr_ty(&method_args[0]).is_floating_point();
        if path.ident.name.as_str() == "exp";
        then {
            span_lint_and_sugg(
                cx,
                IMPRECISE_FLOPS,
                expr.span,
                "(e.pow(x) - 1) can be computed more accurately",
                "consider using",
                format!(
                    "{}.exp_m1()",
                    Sugg::hir(cx, &method_args[0], "..")
                ),
                Applicability::MachineApplicable,
            );
        }
    }
}

fn is_float_mul_expr<'a>(cx: &LateContext<'_>, expr: &'a Expr<'a>) -> Option<(&'a Expr<'a>, &'a Expr<'a>)> {
    if_chain! {
        if let ExprKind::Binary(Spanned { node: BinOpKind::Mul, .. }, ref lhs, ref rhs) = &expr.kind;
        if cx.tables().expr_ty(lhs).is_floating_point();
        if cx.tables().expr_ty(rhs).is_floating_point();
        then {
            return Some((lhs, rhs));
        }
    }

    None
}

// TODO: Fix rust-lang/rust-clippy#4735
fn check_mul_add(cx: &LateContext<'_>, expr: &Expr<'_>) {
    if let ExprKind::Binary(
        Spanned {
            node: BinOpKind::Add, ..
        },
        lhs,
        rhs,
    ) = &expr.kind
    {
        let (recv, arg1, arg2) = if let Some((inner_lhs, inner_rhs)) = is_float_mul_expr(cx, lhs) {
            (inner_lhs, inner_rhs, rhs)
        } else if let Some((inner_lhs, inner_rhs)) = is_float_mul_expr(cx, rhs) {
            (inner_lhs, inner_rhs, lhs)
        } else {
            return;
        };

        span_lint_and_sugg(
            cx,
            SUBOPTIMAL_FLOPS,
            expr.span,
            "multiply and add expressions can be calculated more efficiently and accurately",
            "consider using",
            format!(
                "{}.mul_add({}, {})",
                prepare_receiver_sugg(cx, recv),
                Sugg::hir(cx, arg1, ".."),
                Sugg::hir(cx, arg2, ".."),
            ),
            Applicability::MachineApplicable,
        );
    }
}

/// Returns true iff expr is an expression which tests whether or not
/// test is positive or an expression which tests whether or not test
/// is nonnegative.
/// Used for check-custom-abs function below
fn is_testing_positive(cx: &LateContext<'_>, expr: &Expr<'_>, test: &Expr<'_>) -> bool {
    if let ExprKind::Binary(Spanned { node: op, .. }, left, right) = expr.kind {
        match op {
            BinOpKind::Gt | BinOpKind::Ge => is_zero(cx, right) && are_exprs_equal(cx, left, test),
            BinOpKind::Lt | BinOpKind::Le => is_zero(cx, left) && are_exprs_equal(cx, right, test),
            _ => false,
        }
    } else {
        false
    }
}

/// See [`is_testing_positive`]
fn is_testing_negative(cx: &LateContext<'_>, expr: &Expr<'_>, test: &Expr<'_>) -> bool {
    if let ExprKind::Binary(Spanned { node: op, .. }, left, right) = expr.kind {
        match op {
            BinOpKind::Gt | BinOpKind::Ge => is_zero(cx, left) && are_exprs_equal(cx, right, test),
            BinOpKind::Lt | BinOpKind::Le => is_zero(cx, right) && are_exprs_equal(cx, left, test),
            _ => false,
        }
    } else {
        false
    }
}

fn are_exprs_equal(cx: &LateContext<'_>, expr1: &Expr<'_>, expr2: &Expr<'_>) -> bool {
    SpanlessEq::new(cx).ignore_fn().eq_expr(expr1, expr2)
}

/// Returns true iff expr is some zero literal
fn is_zero(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    match constant_simple(cx, cx.tables(), expr) {
        Some(Constant::Int(i)) => i == 0,
        Some(Constant::F32(f)) => f == 0.0,
        Some(Constant::F64(f)) => f == 0.0,
        _ => false,
    }
}

/// If the two expressions are negations of each other, then it returns
/// a tuple, in which the first element is true iff expr1 is the
/// positive expressions, and the second element is the positive
/// one of the two expressions
/// If the two expressions are not negations of each other, then it
/// returns None.
fn are_negated<'a>(cx: &LateContext<'_>, expr1: &'a Expr<'a>, expr2: &'a Expr<'a>) -> Option<(bool, &'a Expr<'a>)> {
    if let ExprKind::Unary(UnOp::UnNeg, expr1_negated) = &expr1.kind {
        if are_exprs_equal(cx, expr1_negated, expr2) {
            return Some((false, expr2));
        }
    }
    if let ExprKind::Unary(UnOp::UnNeg, expr2_negated) = &expr2.kind {
        if are_exprs_equal(cx, expr1, expr2_negated) {
            return Some((true, expr1));
        }
    }
    None
}

fn check_custom_abs(cx: &LateContext<'_>, expr: &Expr<'_>) {
    if_chain! {
        if let Some((cond, body, Some(else_body))) = higher::if_block(&expr);
        if let ExprKind::Block(block, _) = body.kind;
        if block.stmts.is_empty();
        if let Some(if_body_expr) = block.expr;
        if let ExprKind::Block(else_block, _) = else_body.kind;
        if else_block.stmts.is_empty();
        if let Some(else_body_expr) = else_block.expr;
        if let Some((if_expr_positive, body)) = are_negated(cx, if_body_expr, else_body_expr);
        then {
            let positive_abs_sugg = (
                "manual implementation of `abs` method",
                format!("{}.abs()", Sugg::hir(cx, body, "..")),
            );
            let negative_abs_sugg = (
                "manual implementation of negation of `abs` method",
                format!("-{}.abs()", Sugg::hir(cx, body, "..")),
            );
            let sugg = if is_testing_positive(cx, cond, body) {
                if if_expr_positive {
                    positive_abs_sugg
                } else {
                    negative_abs_sugg
                }
            } else if is_testing_negative(cx, cond, body) {
                if if_expr_positive {
                    negative_abs_sugg
                } else {
                    positive_abs_sugg
                }
            } else {
                return;
            };
            span_lint_and_sugg(
                cx,
                SUBOPTIMAL_FLOPS,
                expr.span,
                sugg.0,
                "try",
                sugg.1,
                Applicability::MachineApplicable,
            );
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for FloatingPointArithmetic {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'_>) {
        if let ExprKind::MethodCall(ref path, _, args, _) = &expr.kind {
            let recv_ty = cx.tables().expr_ty(&args[0]);

            if recv_ty.is_floating_point() {
                match &*path.ident.name.as_str() {
                    "ln" => check_ln1p(cx, expr, args),
                    "log" => check_log_base(cx, expr, args),
                    "powf" => check_powf(cx, expr, args),
                    "powi" => check_powi(cx, expr, args),
                    _ => {},
                }
            }
        } else {
            check_expm1(cx, expr);
            check_mul_add(cx, expr);
            check_custom_abs(cx, expr);
        }
    }
}
