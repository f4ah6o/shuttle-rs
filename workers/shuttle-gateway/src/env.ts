export interface Env {
  DB: D1Database;
  /** Public base URL of the deployed Worker. */
  PUBLIC_URL?: string;
  /** Owner id associated with the bootstrap admin token. */
  ADMIN_OWNER_ID?: string;
  /**
   * Bootstrap admin bearer. Accepted only until the first admin token is minted
   * for the owner, after which it is rejected (genuinely one-time).
   */
  ADMIN_BOOTSTRAP_TOKEN?: string;
}
