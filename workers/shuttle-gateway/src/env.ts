export interface Env {
  DB: D1Database;
  /** Public base URL of the deployed Worker. */
  PUBLIC_URL?: string;
  /** Cloudflare Access team issuer URL, e.g. https://team.cloudflareaccess.com. */
  ACCESS_TEAM_DOMAIN?: string;
  /** Application Audience (AUD) tag for the Access application. */
  ACCESS_APPLICATION_AUD?: string;
  /** Owner id associated with the bootstrap admin token. */
  ADMIN_OWNER_ID?: string;
  /**
   * Bootstrap admin bearer. Accepted only until the first admin token is minted
   * for the owner, after which it is rejected (genuinely one-time).
   */
  ADMIN_BOOTSTRAP_TOKEN?: string;
}
