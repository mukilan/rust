// Copyright 2012-2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/*
 * Inline assembly support.
 */
use self::State::*;

use ast;
use codemap;
use codemap::Span;
use ext::base;
use ext::base::*;
use parse::token::InternedString;
use parse::token;
use ptr::P;

enum State {
    Asm,
    Outputs,
    Inputs,
    Clobbers,
    Options,
    StateNone
}

impl State {
    fn next(&self) -> State {
        match *self {
            Asm       => Outputs,
            Outputs   => Inputs,
            Inputs    => Clobbers,
            Clobbers  => Options,
            Options   => StateNone,
            StateNone => StateNone
        }
    }
}

static OPTIONS: &'static [&'static str] = &["volatile", "alignstack", "intel"];

pub fn expand_asm<'cx>(cx: &'cx mut ExtCtxt, sp: Span, tts: &[ast::TokenTree])
                       -> Box<base::MacResult+'cx> {
    let mut p = cx.new_parser_from_tts(tts);
    let mut asm = InternedString::new("");
    let mut asm_str_style = None;
    let mut outputs = Vec::new();
    let mut inputs = Vec::new();
    let mut clobs = Vec::new();
    let mut volatile = false;
    let mut alignstack = false;
    let mut dialect = ast::AsmAtt;

    let mut state = Asm;

    'statement: loop {
        match state {
            Asm => {
                let (s, style) = match expr_to_string(cx, p.parse_expr(),
                                                   "inline assembly must be a string literal") {
                    Some((s, st)) => (s, st),
                    // let compilation continue
                    None => return DummyResult::expr(sp),
                };
                asm = s;
                asm_str_style = Some(style);
            }
            Outputs => {
                while p.token != token::Eof &&
                      p.token != token::Colon &&
                      p.token != token::ModSep {

                    if outputs.len() != 0 {
                        p.eat(&token::Comma);
                    }

                    let (constraint, _str_style) = p.parse_str();

                    let span = p.last_span;

                    p.expect(&token::OpenDelim(token::Paren));
                    let out = p.parse_expr();
                    p.expect(&token::CloseDelim(token::Paren));

                    // Expands a read+write operand into two operands.
                    //
                    // Use '+' modifier when you want the same expression
                    // to be both an input and an output at the same time.
                    // It's the opposite of '=&' which means that the memory
                    // cannot be shared with any other operand (usually when
                    // a register is clobbered early.)
                    let output = match constraint.get().slice_shift_char() {
                        Some(('=', _)) => None,
                        Some(('+', operand)) => {
                            Some(token::intern_and_get_ident(format!(
                                        "={}",
                                        operand).as_slice()))
                        }
                        _ => {
                            cx.span_err(span, "output operand constraint lacks '=' or '+'");
                            None
                        }
                    };

                    let is_rw = output.is_some();
                    outputs.push((output.unwrap_or(constraint), out, is_rw));
                }
            }
            Inputs => {
                while p.token != token::Eof &&
                      p.token != token::Colon &&
                      p.token != token::ModSep {

                    if inputs.len() != 0 {
                        p.eat(&token::Comma);
                    }

                    let (constraint, _str_style) = p.parse_str();

                    if constraint.get().starts_with("=") {
                        cx.span_err(p.last_span, "input operand constraint contains '='");
                    } else if constraint.get().starts_with("+") {
                        cx.span_err(p.last_span, "input operand constraint contains '+'");
                    }

                    p.expect(&token::OpenDelim(token::Paren));
                    let input = p.parse_expr();
                    p.expect(&token::CloseDelim(token::Paren));

                    inputs.push((constraint, input));
                }
            }
            Clobbers => {
                while p.token != token::Eof &&
                      p.token != token::Colon &&
                      p.token != token::ModSep {

                    if clobs.len() != 0 {
                        p.eat(&token::Comma);
                    }

                    let (s, _str_style) = p.parse_str();

                    if OPTIONS.iter().any(|opt| s.equiv(opt)) {
                        cx.span_warn(p.last_span, "expected a clobber, found an option");
                    }
                    clobs.push(s);
                }
            }
            Options => {
                let (option, _str_style) = p.parse_str();

                if option.equiv(&("volatile")) {
                    // Indicates that the inline assembly has side effects
                    // and must not be optimized out along with its outputs.
                    volatile = true;
                } else if option.equiv(&("alignstack")) {
                    alignstack = true;
                } else if option.equiv(&("intel")) {
                    dialect = ast::AsmIntel;
                } else {
                    cx.span_warn(p.last_span, "unrecognized option");
                }

                if p.token == token::Comma {
                    p.eat(&token::Comma);
                }
            }
            StateNone => ()
        }

        loop {
            // MOD_SEP is a double colon '::' without space in between.
            // When encountered, the state must be advanced twice.
            match (&p.token, state.next(), state.next().next()) {
                (&token::Colon, StateNone, _)   |
                (&token::ModSep, _, StateNone) => {
                    p.bump();
                    break 'statement;
                }
                (&token::Colon, st, _)   |
                (&token::ModSep, _, st) => {
                    p.bump();
                    state = st;
                }
                (&token::Eof, _, _) => break 'statement,
                _ => break
            }
        }
    }

    let expn_id = cx.codemap().record_expansion(codemap::ExpnInfo {
        call_site: sp,
        callee: codemap::NameAndSpan {
            name: "asm".to_string(),
            format: codemap::MacroBang,
            span: None,
        },
    });

    MacExpr::new(P(ast::Expr {
        id: ast::DUMMY_NODE_ID,
        node: ast::ExprInlineAsm(ast::InlineAsm {
            asm: token::intern_and_get_ident(asm.get()),
            asm_str_style: asm_str_style.unwrap(),
            outputs: outputs,
            inputs: inputs,
            clobbers: clobs,
            volatile: volatile,
            alignstack: alignstack,
            dialect: dialect,
            expn_id: expn_id,
        }),
        span: sp
    }))
}
