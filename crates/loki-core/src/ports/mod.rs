//! Ring 1. Versioned.
//!
//! A port exists only where something crosses to the outside world. If both sides of an
//! interface are code we own and will never swap, it is a function call, not a port.
//!
//! Built so far: [`clock`], [`egress`], [`model`], [`tool`].
//!
//! Planned:
//!
//! | Port             | Adapters that will implement it                          |
//! |------------------|----------------------------------------------------------|
//! | `ModelProvider`  | Anthropic, OpenAI, a local MLX actor                     |
//! | `Tool`           | file, EventKit, HTTP, shell, WASM component, MCP client  |
//! | `SearchProvider` | rquest, spider, later Brave or Exa                       |
//! | `Connector`      | Google, GitHub, Notion                                   |
//! | `SearchBackend`  | FTS5, local embeddings                                   |
//! | `SecretStore`    | macOS Keychain                                           |
//! | `EventSink`      | plain renderer, trace renderer, ledger, undo journal     |
//! | `Clock`          | the system clock, a fake clock for tests                 |
//! | `Egress`         | one HTTP client, and a recording proxy in tests          |

pub mod clock;
pub mod egress;
pub mod model;
pub mod search;
pub mod tool;
