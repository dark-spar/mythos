// Mythos is a single-page app embedded in the server binary.
// SSR is disabled because the server is Rust, not Node, and the SPA
// authenticates and fetches data at runtime via the API.
export const ssr = false;
export const prerender = false;
export const trailingSlash = 'never';
