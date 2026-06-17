// Build-time public configuration for Polar licensing.
//
// Every value here is PUBLIC and safe to ship in the app bundle. License
// validation runs against Polar's *customer-portal* endpoints, which require
// only the organization id — never a secret or private API token. See
// jiji-logbook/plans/POLAR_LICENSE_PLAN.md.

/** Polar organization that owns the Jiji products and license-key benefit. */
export const POLAR_ORG_ID = "a37fb037-8fc7-4c87-982f-e9b4720237a8";

/** Customer-portal license-key API base (public, CORS-enabled, no auth). */
export const POLAR_LICENSE_API =
  "https://api.polar.sh/v1/customer-portal/license-keys";

/** Hosted Polar checkout links for the two one-time products. */
export const SOLO_CHECKOUT_URL =
  "https://buy.polar.sh/polar_cl_5MwwECsz2qK0oUEHHLfeXktj9kSxsjlnRWOA60jKFGl";
export const PERSONAL_CHECKOUT_URL =
  "https://buy.polar.sh/polar_cl_2vELWWSZMb7KoKqDjsILXonFNpeHyS1JtwZxg2ixOUw";

/** Prices, for the registration popover copy. */
export const SOLO_PRICE = "$15";
export const PERSONAL_PRICE = "$20";
