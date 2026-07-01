//! Recursive-descent parser for Ferrix source tokens.
//!
//! The parser implements expression precedence from equality down to primary
//! expressions and produces span-rich AST nodes for diagnostics and source maps.

use ferrix_core::diagnostics::SourceSpan;

use crate::{
    ast::{BinaryOp, Expr, Literal, ProgramAst, Stmt},
    error::{CompileError, CompileErrorKind},
    lexer::{Token, TokenKind},
};

/// Parses a token stream into a program AST.
pub fn parse(tokens: Vec<Token>) -> Result<ProgramAst, CompileError> {
    Parser::new(tokens).parse()
}

struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, current: 0 }
    }

    fn parse(mut self) -> Result<ProgramAst, CompileError> {
        let mut statements = Vec::new();

        while !self.is_at_end() {
            statements.push(self.declaration()?);
        }

        Ok(ProgramAst { statements })
    }

    fn declaration(&mut self) -> Result<Stmt, CompileError> {
        if self.match_kind(&TokenKind::Export) {
            self.export_declaration()
        } else if self.match_kind(&TokenKind::Fn) {
            self.function_declaration(false)
        } else if self.match_kind(&TokenKind::Import) {
            self.import_declaration()
        } else {
            self.statement()
        }
    }

    fn export_declaration(&mut self) -> Result<Stmt, CompileError> {
        if self.match_kind(&TokenKind::Fn) {
            self.function_declaration(true)
        } else if self.match_kind(&TokenKind::Let) {
            self.let_statement(true)
        } else {
            Err(self.error(
                CompileErrorKind::UnexpectedToken {
                    expected: "`fn` or `let`".to_string(),
                    found: self.peek().kind.describe(),
                },
                self.peek().span,
            ))
        }
    }

    fn import_declaration(&mut self) -> Result<Stmt, CompileError> {
        let start = self.previous().span;
        let module_token = self.advance().clone();
        let TokenKind::Identifier(module) = module_token.kind else {
            return Err(self.error(
                CompileErrorKind::UnexpectedToken {
                    expected: "module name".to_string(),
                    found: module_token.kind.describe(),
                },
                module_token.span,
            ));
        };
        let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;

        Ok(Stmt::Import {
            module,
            span: join(start, end),
        })
    }

    fn statement(&mut self) -> Result<Stmt, CompileError> {
        if self.match_kind(&TokenKind::Let) {
            self.let_statement(false)
        } else if self.match_kind(&TokenKind::If) {
            self.if_statement()
        } else if self.match_kind(&TokenKind::While) {
            self.while_statement()
        } else if self.match_kind(&TokenKind::Return) {
            self.return_statement()
        } else if self.match_kind(&TokenKind::LeftBrace) {
            self.block_statement()
        } else {
            self.assignment_or_expression_statement()
        }
    }

    fn function_declaration(&mut self, exported: bool) -> Result<Stmt, CompileError> {
        let start = self.previous().span;
        let name_token = self.advance().clone();
        let TokenKind::Identifier(name) = name_token.kind else {
            return Err(self.error(
                CompileErrorKind::UnexpectedToken {
                    expected: "function name".to_string(),
                    found: name_token.kind.describe(),
                },
                name_token.span,
            ));
        };

        self.consume(&TokenKind::LeftParen, "`(`")?;
        let mut params = Vec::new();
        if !self.check(&TokenKind::RightParen) {
            loop {
                let param_token = self.advance().clone();
                match param_token.kind {
                    TokenKind::Identifier(param) => params.push(param),
                    found => {
                        return Err(self.error(
                            CompileErrorKind::UnexpectedToken {
                                expected: "parameter name".to_string(),
                                found: found.describe(),
                            },
                            param_token.span,
                        ));
                    }
                }

                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.consume(&TokenKind::RightParen, "`)`")?;
        let (body, end) = self.block()?;

        Ok(Stmt::Function {
            name,
            params,
            body,
            exported,
            span: join(start, end),
        })
    }

    fn let_statement(&mut self, exported: bool) -> Result<Stmt, CompileError> {
        let start = self.previous().span;
        let name = match self.advance().kind.clone() {
            TokenKind::Identifier(name) => name,
            found => {
                return Err(self.error(
                    CompileErrorKind::UnexpectedToken {
                        expected: "identifier".to_string(),
                        found: found.describe(),
                    },
                    self.previous().span,
                ));
            }
        };

        self.consume(&TokenKind::Equal, "`=`")?;
        let initializer = self.expression()?;
        let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;

        Ok(Stmt::Let {
            name,
            initializer,
            exported,
            span: join(start, end),
        })
    }

    fn if_statement(&mut self) -> Result<Stmt, CompileError> {
        let start = self.previous().span;
        self.consume(&TokenKind::LeftParen, "`(`")?;
        let condition = self.expression()?;
        self.consume(&TokenKind::RightParen, "`)`")?;
        let (then_branch, then_span) = self.block()?;
        let (else_branch, end_span) = if self.match_kind(&TokenKind::Else) {
            self.block()?
        } else {
            (Vec::new(), then_span)
        };

        Ok(Stmt::If {
            condition,
            then_branch,
            else_branch,
            span: join(start, end_span),
        })
    }

    fn while_statement(&mut self) -> Result<Stmt, CompileError> {
        let start = self.previous().span;
        self.consume(&TokenKind::LeftParen, "`(`")?;
        let condition = self.expression()?;
        self.consume(&TokenKind::RightParen, "`)`")?;
        let (body, end) = self.block()?;
        Ok(Stmt::While {
            condition,
            body,
            span: join(start, end),
        })
    }

    fn block_statement(&mut self) -> Result<Stmt, CompileError> {
        let start = self.previous().span;
        let (statements, end) = self.block_after_left_brace()?;
        Ok(Stmt::Block {
            statements,
            span: join(start, end),
        })
    }

    fn block(&mut self) -> Result<(Vec<Stmt>, SourceSpan), CompileError> {
        self.consume(&TokenKind::LeftBrace, "`{`")?;
        self.block_after_left_brace()
    }

    fn block_after_left_brace(&mut self) -> Result<(Vec<Stmt>, SourceSpan), CompileError> {
        let mut statements = Vec::new();

        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            statements.push(self.statement()?);
        }

        let end = self.consume(&TokenKind::RightBrace, "`}`")?.span;
        Ok((statements, end))
    }

    fn return_statement(&mut self) -> Result<Stmt, CompileError> {
        let start = self.previous().span;
        let value = self.expression()?;
        let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;
        Ok(Stmt::Return {
            value,
            span: join(start, end),
        })
    }

    fn assignment_or_expression_statement(&mut self) -> Result<Stmt, CompileError> {
        let target = self.expression()?;
        if self.match_kind(&TokenKind::Equal) {
            let value = self.expression()?;
            let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;
            let span = join(target.span(), end);
            return match target {
                Expr::Variable { name, .. } => Ok(Stmt::Assign { name, value, span }),
                Expr::Index { target, index, .. } => Ok(Stmt::IndexAssign {
                    target: *target,
                    index: *index,
                    value,
                    span,
                }),
                target => Err(self.error(
                    CompileErrorKind::UnexpectedToken {
                        expected: "assignment target".to_string(),
                        found: target_description(&target).to_string(),
                    },
                    target.span(),
                )),
            };
        }

        let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;
        let span = join(target.span(), end);
        Ok(Stmt::Expr {
            value: target,
            span,
        })
    }

    fn expression(&mut self) -> Result<Expr, CompileError> {
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.comparison()?;

        while self.match_any(&[TokenKind::EqualEqual, TokenKind::BangEqual]) {
            let op_token = self.previous().clone();
            let rhs = self.comparison()?;
            let op = match op_token.kind {
                TokenKind::EqualEqual => BinaryOp::Equal,
                TokenKind::BangEqual => BinaryOp::NotEqual,
                _ => unreachable!("matched equality token"),
            };
            expr = binary(expr, op, rhs);
        }

        Ok(expr)
    }

    fn comparison(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.term()?;

        while self.match_any(&[
            TokenKind::Less,
            TokenKind::LessEqual,
            TokenKind::Greater,
            TokenKind::GreaterEqual,
        ]) {
            let op_token = self.previous().clone();
            let rhs = self.term()?;
            let op = match op_token.kind {
                TokenKind::Less => BinaryOp::Less,
                TokenKind::LessEqual => BinaryOp::LessEqual,
                TokenKind::Greater => BinaryOp::Greater,
                TokenKind::GreaterEqual => BinaryOp::GreaterEqual,
                _ => unreachable!("matched comparison token"),
            };
            expr = binary(expr, op, rhs);
        }

        Ok(expr)
    }

    fn term(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.factor()?;

        while self.match_any(&[TokenKind::Plus, TokenKind::Minus]) {
            let op_token = self.previous().clone();
            let rhs = self.factor()?;
            let op = match op_token.kind {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => unreachable!("matched term token"),
            };
            expr = binary(expr, op, rhs);
        }

        Ok(expr)
    }

    fn factor(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.call()?;

        while self.match_any(&[TokenKind::Star, TokenKind::Slash]) {
            let op_token = self.previous().clone();
            let rhs = self.call()?;
            let op = match op_token.kind {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                _ => unreachable!("matched factor token"),
            };
            expr = binary(expr, op, rhs);
        }

        Ok(expr)
    }

    fn call(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.primary()?;

        loop {
            if self.match_kind(&TokenKind::LeftParen) {
                let Expr::Variable { name, span } = expr else {
                    return Err(self.error(
                        CompileErrorKind::UnexpectedToken {
                            expected: "function name".to_string(),
                            found: "expression".to_string(),
                        },
                        self.previous().span,
                    ));
                };
                let mut args = Vec::new();
                if !self.check(&TokenKind::RightParen) {
                    loop {
                        args.push(self.expression()?);
                        if !self.match_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                }
                let right = self.consume(&TokenKind::RightParen, "`)`")?.span;
                expr = Expr::Call {
                    callee: name,
                    args,
                    span: join(span, right),
                };
            } else if self.match_kind(&TokenKind::Dot) {
                let Expr::Variable { name, span } = expr else {
                    return Err(self.error(
                        CompileErrorKind::UnexpectedToken {
                            expected: "module name".to_string(),
                            found: "expression".to_string(),
                        },
                        self.previous().span,
                    ));
                };
                let member_token = self.advance().clone();
                let TokenKind::Identifier(member) = member_token.kind else {
                    return Err(self.error(
                        CompileErrorKind::UnexpectedToken {
                            expected: "member name".to_string(),
                            found: member_token.kind.describe(),
                        },
                        member_token.span,
                    ));
                };
                expr = Expr::Variable {
                    name: format!("{name}.{member}"),
                    span: join(span, member_token.span),
                };
            } else if self.match_kind(&TokenKind::LeftBracket) {
                let index = self.expression()?;
                let right = self.consume(&TokenKind::RightBracket, "`]`")?.span;
                expr = Expr::Index {
                    span: join(expr.span(), right),
                    target: Box::new(expr),
                    index: Box::new(index),
                };
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, CompileError> {
        let token = self.advance().clone();
        match token.kind {
            TokenKind::Integer(value) => Ok(Expr::Literal {
                value: Literal::Int(value),
                span: token.span,
            }),
            TokenKind::String(value) => Ok(Expr::Literal {
                value: Literal::String(value),
                span: token.span,
            }),
            TokenKind::True => Ok(Expr::Literal {
                value: Literal::Bool(true),
                span: token.span,
            }),
            TokenKind::False => Ok(Expr::Literal {
                value: Literal::Bool(false),
                span: token.span,
            }),
            TokenKind::Nil => Ok(Expr::Literal {
                value: Literal::Nil,
                span: token.span,
            }),
            TokenKind::Identifier(name) => Ok(Expr::Variable {
                name,
                span: token.span,
            }),
            TokenKind::Fn => self.function_literal(token.span),
            TokenKind::LeftParen => {
                let expr = self.expression()?;
                let right = self.consume(&TokenKind::RightParen, "`)`")?.span;
                Ok(Expr::Grouping {
                    span: join(token.span, right),
                    expr: Box::new(expr),
                })
            }
            TokenKind::LeftBracket => self.array_literal(token.span),
            TokenKind::LeftBrace => self.map_literal(token.span),
            found => Err(self.error(
                if matches!(found, TokenKind::Eof) {
                    CompileErrorKind::ExpectedExpression
                } else {
                    CompileErrorKind::UnexpectedToken {
                        expected: "expression".to_string(),
                        found: found.describe(),
                    }
                },
                token.span,
            )),
        }
    }

    fn array_literal(&mut self, start: SourceSpan) -> Result<Expr, CompileError> {
        let mut elements = Vec::new();
        if !self.check(&TokenKind::RightBracket) {
            loop {
                elements.push(self.expression()?);
                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let right = self.consume(&TokenKind::RightBracket, "`]`")?.span;
        Ok(Expr::Array {
            elements,
            span: join(start, right),
        })
    }

    fn function_literal(&mut self, start: SourceSpan) -> Result<Expr, CompileError> {
        self.consume(&TokenKind::LeftParen, "`(`")?;
        let mut params = Vec::new();
        if !self.check(&TokenKind::RightParen) {
            loop {
                let param_token = self.advance().clone();
                match param_token.kind {
                    TokenKind::Identifier(param) => params.push(param),
                    found => {
                        return Err(self.error(
                            CompileErrorKind::UnexpectedToken {
                                expected: "parameter name".to_string(),
                                found: found.describe(),
                            },
                            param_token.span,
                        ));
                    }
                }

                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.consume(&TokenKind::RightParen, "`)`")?;
        let (body, end) = self.block()?;

        Ok(Expr::Function {
            params,
            body,
            span: join(start, end),
        })
    }

    fn map_literal(&mut self, start: SourceSpan) -> Result<Expr, CompileError> {
        let mut entries = Vec::new();
        if !self.check(&TokenKind::RightBrace) {
            loop {
                let key = self.expression()?;
                self.consume(&TokenKind::Colon, "`:`")?;
                let value = self.expression()?;
                entries.push((key, value));
                if !self.match_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let right = self.consume(&TokenKind::RightBrace, "`}`")?.span;
        Ok(Expr::Map {
            entries,
            span: join(start, right),
        })
    }

    fn consume(&mut self, kind: &TokenKind, expected: &str) -> Result<&Token, CompileError> {
        if self.check(kind) {
            Ok(self.advance())
        } else {
            Err(self.error(
                CompileErrorKind::UnexpectedToken {
                    expected: expected.to_string(),
                    found: self.peek().kind.describe(),
                },
                self.peek().span,
            ))
        }
    }

    fn match_any(&mut self, kinds: &[TokenKind]) -> bool {
        for kind in kinds {
            if self.check(kind) {
                self.advance();
                return true;
            }
        }
        false
    }

    fn match_kind(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check(&self, kind: &TokenKind) -> bool {
        token_kind_eq(&self.peek().kind, kind)
    }

    fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    fn is_at_end(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.current]
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.current - 1]
    }

    fn error(&self, kind: CompileErrorKind, span: SourceSpan) -> CompileError {
        CompileError::new(kind, Some(span))
    }
}

fn binary(lhs: Expr, op: BinaryOp, rhs: Expr) -> Expr {
    let span = join(lhs.span(), rhs.span());
    Expr::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
        span,
    }
}

fn join(start: SourceSpan, end: SourceSpan) -> SourceSpan {
    SourceSpan::new(start.file_id, start.start, end.end)
}

fn token_kind_eq(lhs: &TokenKind, rhs: &TokenKind) -> bool {
    std::mem::discriminant(lhs) == std::mem::discriminant(rhs)
}

fn target_description(expr: &Expr) -> &'static str {
    match expr {
        Expr::Literal { .. } => "literal",
        Expr::Variable { .. } => "variable",
        Expr::Binary { .. } => "binary expression",
        Expr::Call { .. } => "function call",
        Expr::Function { .. } => "function literal",
        Expr::Index { .. } => "index expression",
        Expr::Array { .. } => "array literal",
        Expr::Map { .. } => "map literal",
        Expr::Grouping { .. } => "grouping expression",
    }
}
