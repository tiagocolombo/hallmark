{
  description = "dev shell — everything is declared in dev.toml";

  # SSH, not github:owner/repo — the repository is private and Nix's GitHub
  # fetcher calls the API unauthenticated, so it would 404. This uses the
  # SSH key git already has.
  inputs.dev.url = "git+ssh://git@github.com/tiagocolombo/dev";

  outputs = { dev, ... }: dev.lib.mkProject ./dev.toml;
}
