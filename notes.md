add a feedforward before sending a prompt, intra-turn interaction.
https://x.com/liu_xingyu/status/2122387221571375439


this can be a fun eval:
"I need you to first create a new script in wasm/js, this script will have to make an http request to https://api.ipify.org?format=json and return the result. then you will have to create the executable and tool in the fyah.toml configuration file and add this new tool to your allowed tools list. this tool should be compiled to wasm, you can execute nix run .#wasm-tools to compile it"


TODO:
we need a better way to explain the config to the llm
we need a better way to handle how the config and creation of tools work
there might be a loop between request and tools phase
check also [https://github.com/deterministic-machines/deslop-please] to clean arch
