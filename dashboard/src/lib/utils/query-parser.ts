/**
 * Frontend Query Parser for Sauron Search Engine.
 * Parses search query strings with boolean logic (AND, OR, NOT, parentheses),
 * predicate operators (=, !=, >, >=, <, <=, in, has, like, contains),
 * and variable prefixes (@tag, @context, @extra, @$label) into AST nodes.
 * Compatible with backend sauron_query::ast::Node.
 */

export type MatchOp =
  | 'Eq'
  | 'Ne'
  | 'Gt'
  | 'Gte'
  | 'Lt'
  | 'Lte'
  | 'In'
  | 'Has'
  | 'Like'
  | 'Contains';

export interface Predicate {
  field: string;
  value: string;
  quoted: boolean;
  at: number;
}

export type QueryNode =
  | { And: QueryNode[] }
  | { Or: QueryNode[] }
  | { Not: QueryNode }
  | { Pred: Predicate }
  | { Text: string };

export class QueryParseError extends Error {
  constructor(message: string, public at: number = 0) {
    super(message);
    this.name = 'QueryParseError';
  }
}

type TokenType =
  | 'LParen'
  | 'RParen'
  | 'And'
  | 'Or'
  | 'Not'
  | 'Pred'
  | 'Text';

interface Token {
  type: TokenType;
  field?: string;
  value?: string;
  quoted?: boolean;
  text?: string;
  at: number;
}

/**
 * Tokenize input string into token stream.
 */
export function lex(input: string): Token[] {
  const tokens: Token[] = [];
  let pos = 0;
  const len = input.length;

  while (pos < len) {
    // Skip whitespace
    if (/\s/.test(input[pos])) {
      pos++;
      continue;
    }

    const at = pos;

    // Parentheses
    if (input[pos] === '(') {
      tokens.push({ type: 'LParen', at: pos });
      pos++;
      continue;
    }
    if (input[pos] === ')') {
      tokens.push({ type: 'RParen', at: pos });
      pos++;
      continue;
    }

    // Bang as NOT
    if (input[pos] === '!') {
      // Check if it's != operator inside a term or standalone !
      // If next char is not '=', it's unary NOT
      if (pos + 1 >= len || input[pos + 1] !== '=') {
        tokens.push({ type: 'Not', at: pos });
        pos++;
        continue;
      }
    }

    // Quoted string
    if (input[pos] === '"' || input[pos] === "'") {
      const quoteChar = input[pos];
      pos++;
      let start = pos;
      let str = '';
      let closed = false;
      while (pos < len) {
        if (input[pos] === '\\' && pos + 1 < len) {
          str += input[pos + 1];
          pos += 2;
        } else if (input[pos] === quoteChar) {
          closed = true;
          pos++;
          break;
        } else {
          str += input[pos];
          pos++;
        }
      }
      if (!closed) {
        throw new QueryParseError(`Unclosed quote starting at index ${at}`, at);
      }
      // Quoted string can be text node or predicate value depending on context
      tokens.push({ type: 'Text', text: str, at });
      continue;
    }

    // Identifiers, keywords, or predicates
    let wordStart = pos;
    while (pos < len && !/\s/.test(input[pos]) && input[pos] !== '(' && input[pos] !== ')') {
      // If we see quotes inside, consume quote sequence
      if (input[pos] === '"' || input[pos] === "'") {
        const q = input[pos];
        pos++;
        while (pos < len && input[pos] !== q) {
          if (input[pos] === '\\') pos++;
          pos++;
        }
        if (pos < len) pos++; // closing quote
      } else {
        pos++;
      }
    }

    const word = input.slice(wordStart, pos);
    const upperWord = word.toUpperCase();

    // Keywords
    if (upperWord === 'AND' || upperWord === '&&') {
      tokens.push({ type: 'And', at: wordStart });
      continue;
    }
    if (upperWord === 'OR' || upperWord === '||') {
      tokens.push({ type: 'Or', at: wordStart });
      continue;
    }
    if (upperWord === 'NOT') {
      tokens.push({ type: 'Not', at: wordStart });
      continue;
    }

    // Check for predicate pattern (e.g., field=val, field!=val, field>val, field:val, etc.)
    // Supports @tag=v1, @context.app_version=3.0.2, @extra.level=warn, @$label.team=frontend, field!=val, field in [a,b], has:field
    const predToken = parseWordToToken(word, wordStart);
    tokens.push(predToken);
  }

  return tokens;
}

function parseWordToToken(word: string, at: number): Token {
  // Check for `has:field` or `has field`
  if (word.startsWith('has:') || word.startsWith('has=')) {
    const val = word.slice(4);
    return { type: 'Pred', field: 'has', value: val, quoted: false, at };
  }

  // Check for `contains:value`
  if (word.startsWith('contains:')) {
    const val = word.slice(9);
    return { type: 'Pred', field: 'contains', value: val, quoted: false, at };
  }

  // Operator regex matching field<op>value
  // Ops: !=, >=, <=, =, :, >, <, in
  // Match inequality != first before =
  const matchNe = word.match(/^([@$\w.-]+)!=(.+)$/);
  if (matchNe) {
    return { type: 'Pred', field: matchNe[1], value: `!=${matchNe[2]}`, quoted: false, at };
  }

  const matchGte = word.match(/^([@$\w.-]+)>=(.+)$/);
  if (matchGte) {
    return { type: 'Pred', field: matchGte[1], value: `>=${matchGte[2]}`, quoted: false, at };
  }

  const matchLte = word.match(/^([@$\w.-]+)<=(.+)$/);
  if (matchLte) {
    return { type: 'Pred', field: matchLte[1], value: `<=${matchLte[2]}`, quoted: false, at };
  }

  const matchGt = word.match(/^([@$\w.-]+)>(.+)$/);
  if (matchGt) {
    return { type: 'Pred', field: matchGt[1], value: `>${matchGt[2]}`, quoted: false, at };
  }

  const matchLt = word.match(/^([@$\w.-]+)<(.+)$/);
  if (matchLt) {
    return { type: 'Pred', field: matchLt[1], value: `<${matchLt[2]}`, quoted: false, at };
  }

  const matchEq = word.match(/^([@$\w.-]+)[=:](.+)$/);
  if (matchEq) {
    let val = matchEq[2];
    let quoted = false;
    if ((val.startsWith('"') && val.endsWith('"')) || (val.startsWith("'") && val.endsWith("'"))) {
      val = val.slice(1, -1);
      quoted = true;
    }
    return { type: 'Pred', field: matchEq[1], value: val, quoted, at };
  }

  // Default to Text token
  return { type: 'Text', text: word, at };
}

/**
 * Parser for token stream using recursive descent.
 */
class Parser {
  private pos = 0;

  constructor(private tokens: Token[]) {}

  private peek(): Token | undefined {
    return this.tokens[this.pos];
  }

  private bump(): Token | undefined {
    const t = this.tokens[this.pos];
    if (t) this.pos++;
    return t;
  }

  public parse(): QueryNode {
    if (this.tokens.length === 0) {
      return { And: [] };
    }

    const node = this.orExpr();

    if (this.pos < this.tokens.length) {
      const tok = this.peek();
      throw new QueryParseError(
        `Unexpected token '${tok?.type}' at position ${tok?.at ?? 0}`,
        tok?.at ?? 0
      );
    }

    return node;
  }

  private orExpr(): QueryNode {
    const branches: QueryNode[] = [this.andExpr()];

    while (this.peek()?.type === 'Or') {
      const orTok = this.bump()!;
      if (!this.peek() || this.atExprEnd()) {
        throw new QueryParseError(`Dangling OR keyword at index ${orTok.at}`, orTok.at);
      }
      branches.push(this.andExpr());
    }

    return flatten({ Or: branches });
  }

  private andExpr(): QueryNode {
    const parts: QueryNode[] = [this.unaryExpr()];

    while (this.pos < this.tokens.length && !this.atExprEnd()) {
      if (this.peek()?.type === 'And') {
        const andTok = this.bump()!;
        if (!this.peek() || this.atExprEnd()) {
          throw new QueryParseError(`Dangling AND keyword at index ${andTok.at}`, andTok.at);
        }
        parts.push(this.unaryExpr());
      } else {
        parts.push(this.unaryExpr());
      }
    }

    return flatten({ And: parts });
  }

  private atExprEnd(): boolean {
    const tok = this.peek();
    return !tok || tok.type === 'Or' || tok.type === 'RParen';
  }

  private unaryExpr(): QueryNode {
    if (this.peek()?.type === 'Not') {
      const notTok = this.bump()!;
      if (!this.peek() || this.atExprEnd()) {
        throw new QueryParseError(`Dangling NOT at index ${notTok.at}`, notTok.at);
      }
      return { Not: this.unaryExpr() };
    }
    return this.primaryExpr();
  }

  private primaryExpr(): QueryNode {
    const tok = this.bump();
    if (!tok) {
      throw new QueryParseError('Unexpected end of input');
    }

    if (tok.type === 'LParen') {
      const inner = this.orExpr();
      const next = this.bump();
      if (next?.type !== 'RParen') {
        throw new QueryParseError(`Unmatched opening parenthesis at index ${tok.at}`, tok.at);
      }
      return inner;
    }

    if (tok.type === 'RParen') {
      throw new QueryParseError(`Unmatched closing parenthesis at index ${tok.at}`, tok.at);
    }

    if (tok.type === 'Pred') {
      // Check if it's inequality predicate `!=`
      if (tok.value?.startsWith('!=')) {
        const rawVal = tok.value.slice(2);
        return {
          Not: {
            Pred: {
              field: tok.field!,
              value: rawVal,
              quoted: tok.quoted ?? false,
              at: tok.at,
            },
          },
        };
      }

      return {
        Pred: {
          field: tok.field!,
          value: tok.value!,
          quoted: tok.quoted ?? false,
          at: tok.at,
        },
      };
    }

    if (tok.type === 'Text') {
      return { Text: tok.text! };
    }

    throw new QueryParseError(`Unexpected token at index ${tok.at}`, tok.at);
  }
}

/**
 * Flattens single child And / Or nodes so AST stays minimal.
 */
function flatten(node: QueryNode): QueryNode {
  if ('And' in node && node.And.length === 1) {
    return node.And[0];
  }
  if ('Or' in node && node.Or.length === 1) {
    return node.Or[0];
  }
  return node;
}

/**
 * Parse a raw query string into a QueryNode AST.
 */
export function parseQuery(input: string): QueryNode {
  const trimmed = input.trim();
  if (!trimmed) {
    return { And: [] };
  }
  const tokens = lex(trimmed);
  const parser = new Parser(tokens);
  return parser.parse();
}
