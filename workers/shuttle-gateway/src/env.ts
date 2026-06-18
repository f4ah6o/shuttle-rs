export interface Env {
  DB: D1Database;
  /** Public base URL of the deployed Worker. */
  PUBLIC_URL?: string;
  /** Owner id associated with the bootstrap admin token. */
  ADMIN_OWNER_ID?: string;
  /** One-time admin bearer used to create projects and mint scoped tokens. */
  ADMIN_BOOTSTRAP_TOKEN?: string;
}
