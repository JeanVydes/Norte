# cookbook

Small recipes for specific Norte tasks. Nothing is shared between them, so a
recipe can be read — and copied — without the rest of the crate.

Every recipe uses `norte-core` alone: they speak to the `Model` port or the
turn loop directly, without an engine. `tools` never calls a model at all.

## Run

```sh
OPENROUTER_API_KEY=sk-or-... \
COOKBOOK_MODEL=deepseek/deepseek-v4.1-flash-1017 \
COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
cargo run -p norte-cookbook --example model
```

The key comes from the environment; a local `.env` works with
`set -a; . ./.env; set +a`. `tools` needs no key and calls no model.

## Recipes

| Example | What it shows |
| --- | --- |
| `model` | One request against a real provider, every event printed. |
| `turn` | The turn loop reduced to its bones: history as a `Vec<Message>`. |
| `tool_loop` | A complete tool loop: the recipe a host copies to write its own. |
| `chat` | A whole conversation: several turns, tools and a bounded retry. |
| `tools` | Declaring and running tools, no model involved. |
| `layer` | A layer around a model call. |
| `image_in` | Sending an image to a model. |
| `image_out` | Receiving an image back and storing it. |

The cookbook is not a workspace member: build it on its own with
`cargo run -p norte-cookbook --example <name>` from this directory.
