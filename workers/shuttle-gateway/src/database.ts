/**
 * Storage boundary for the gateway.
 *
 * Application services depend only on this narrow port, never on D1 directly,
 * so a different server database (or an in-memory test database) can be
 * introduced without changing MCP/API semantics.
 */
export type Row = Record<string, unknown>;

export interface Statement {
  sql: string;
  params?: unknown[];
}

export interface Database {
  /** Run a query returning every matching row. */
  query<T extends Row = Row>(sql: string, params?: unknown[]): Promise<T[]>;
  /** Run a query returning the first row, or null. */
  first<T extends Row = Row>(sql: string, params?: unknown[]): Promise<T | null>;
  /** Run a statement for its side effects. */
  run(sql: string, params?: unknown[]): Promise<void>;
  /** Run several statements atomically. */
  batch(statements: Statement[]): Promise<void>;
}

/** Adapts a Cloudflare D1 binding to the {@link Database} port. */
export class D1Database_ implements Database {
  constructor(private readonly db: D1Database) {}

  async query<T extends Row = Row>(sql: string, params: unknown[] = []): Promise<T[]> {
    const result = await this.db
      .prepare(sql)
      .bind(...params)
      .all<T>();
    return result.results ?? [];
  }

  async first<T extends Row = Row>(sql: string, params: unknown[] = []): Promise<T | null> {
    return (await this.db
      .prepare(sql)
      .bind(...params)
      .first<T>()) as T | null;
  }

  async run(sql: string, params: unknown[] = []): Promise<void> {
    await this.db
      .prepare(sql)
      .bind(...params)
      .run();
  }

  async batch(statements: Statement[]): Promise<void> {
    const prepared = statements.map((statement) =>
      this.db.prepare(statement.sql).bind(...(statement.params ?? [])),
    );
    await this.db.batch(prepared);
  }
}
