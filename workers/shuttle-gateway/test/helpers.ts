import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import type { Database, Row, Statement } from "../src/database.js";

// node:sqlite is a recent built-in; load it at runtime so the test bundler does
// not try to statically resolve it.
const { DatabaseSync } = createRequire(import.meta.url)("node:sqlite") as {
  DatabaseSync: new (path: string) => {
    exec(sql: string): void;
    prepare(sql: string): {
      all(...params: unknown[]): unknown[];
      get(...params: unknown[]): unknown;
      run(...params: unknown[]): unknown;
    };
  };
};

const here = dirname(fileURLToPath(import.meta.url));

/** A {@link Database} implementation over Node's built-in SQLite for tests. */
export class NodeSqliteDatabase implements Database {
  private readonly db: InstanceType<typeof DatabaseSync>;

  constructor() {
    this.db = new DatabaseSync(":memory:");
    for (const migration of ["0001_init.sql", "0002_cloud_first.sql"]) {
      this.db.exec(readFileSync(join(here, "..", "migrations", migration), "utf8"));
    }
  }

  async query<T extends Row = Row>(sql: string, params: unknown[] = []): Promise<T[]> {
    return this.db.prepare(sql).all(...(params as never[])) as T[];
  }

  async first<T extends Row = Row>(sql: string, params: unknown[] = []): Promise<T | null> {
    const row = this.db.prepare(sql).get(...(params as never[]));
    return (row as T | undefined) ?? null;
  }

  async run(sql: string, params: unknown[] = []): Promise<number> {
    const result = this.db.prepare(sql).run(...(params as never[])) as { changes?: number };
    return result.changes ?? 0;
  }

  async batch(statements: Statement[]): Promise<void> {
    this.db.exec("BEGIN");
    try {
      for (const statement of statements) {
        this.db.prepare(statement.sql).run(...((statement.params ?? []) as never[]));
      }
      this.db.exec("COMMIT");
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }
}

export function makeRequest(
  method: string,
  path: string,
  options: { token?: string; body?: unknown; headers?: Record<string, string> } = {},
): Request {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    ...(options.headers ?? {}),
  };
  if (options.token) headers.authorization = `Bearer ${options.token}`;
  return new Request(`https://gateway.test${path}`, {
    method,
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
}
