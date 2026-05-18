# zed-workspaces

Tiny CLI for creating and opening `.code-workspace` and `.zed-workspace` files in Zed.

## Install

```sh
cargo install --git https://github.com/fu5ha/zed-workspaces
```

Requires the `zed` CLI to be available on `PATH`.

## Usage

### Create workspace

Create a workspace:

```sh
zed-workspaces create --name my-workspace dir-a dir-b
```

If `--name` or directories are omitted, you will be prompted.

### Open workspace

Open a workspace file:

```sh
zed-workspaces open my-workspace.zed-workspace
```

Reuse the current Zed window:

```sh
zed-workspaces open --reuse my-workspace.zed-workspace
```

Open from the current directory:

```sh
zed-workspaces open
```

This is the same as:

```sh
zed-workspaces open .
```

When opening a directory, it looks for `.zed-workspace` files directly inside that directory. If none exist, it falls back to `.code-workspace` files.

`.zed-workspace` files use the same folder-list shape as `.code-workspace` files. See schema in [`resources/zed-workspace-schema.json`](./resources/zed-workspace-schema.json).

### Install Zed tasks

Install Zed tasks:

```sh
zed-workspaces install
```

This adds or updates the bundled `zed-workspaces` tasks and JSONC file associations in your user-level Zed config.
