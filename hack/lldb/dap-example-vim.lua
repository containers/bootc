-- This is an example of how to configure the DAP connection in an editor (neovim in this case)
-- It should be relatively straightforward to adapt to a different editor

local dap = require("dap")
local job = require("plenary.job")

-- This is a coroutine that runs the cargo build command and reports progress
local program = function()
  return coroutine.create(function(dap_run_co)
    local progress = require("fidget.progress")

    local cargo_build_fidget = progress.handle.create({
      title = "cargo build",
      lsp_client = { name = "Debugger" },
      percentage = 0,
    })

    local cargo_build_job = job:new({
      command = "cargo",
      args = { "build", "--color=never", "--profile=dev" },
      cwd = vim.fn.getcwd(),
      enable_handlers = true,
      on_stderr = vim.schedule_wrap(function(_, output)
        cargo_build_fidget:report({
          message = output,
          percentage = cargo_build_fidget.percentage + 0.3,
        })
      end),
      on_exit = function(_, return_val)
        vim.schedule(function()
          if return_val ~= 0 then
            cargo_build_fidget:report({
              message = "Error during cargo build",
              percentage = 100,
            })
          else
            cargo_build_fidget:finish()
            coroutine.resume(dap_run_co, vim.fn.getcwd() .. "/target/debug/bootc")
          end
        end)
      end,
    })

    cargo_build_job:start()
  end)
end

dap.adapters = {
  lldb = {
    executable = {
      args = {
        "--liblldb",
        "~/.local/share/nvim/mason/packages/codelldb/extension/lldb/lib/liblldb.so",
        "--port",
        "${port}",
      },
      command = "~/.local/share/nvim/mason/packages/codelldb/extension/adapter/codelldb",
    },
    host = "127.0.0.1",
    port = "${port}",
    type = "server",
  },
}

-- rust config that runs cargo build before opening dap ui and starting Debugger
-- shows cargo build status as fidget progress
-- the newly built bootc binary is copied to the VM and run in the lldb-server
dap.configurations.rust = {
  {
    args = { "status" },
    cwd = "/",
    name = "[remote] status",
    program = program,
    request = "launch",
    console = "integratedTerminal",
    stopOnEntry = false,
    type = "lldb",
    initCommands = {
      "platform select remote-linux",
      "platform connect connect://bootc-lldb:1234", -- connect to the lldb-server running in the VM
      "file target/debug/bootc",
    },
  },
  {
    args = { "upgrade" },
    cwd = "/",
    name = "[remote] upgrade",
    program = program,
    request = "launch",
    console = "integratedTerminal",
    stopOnEntry = false,
    type = "lldb",
    initCommands = {
      "platform select remote-linux",
      "platform connect connect://bootc-lldb:1234",
      "file target/debug/bootc",
    },
  },

  -- The install command can connect to a container instead of a VM.
  -- The following command is an example of how to run a container and start a lldb-server:
  -- sudo podman run --pid=host --network=host --privileged --security-opt label=type:unconfined_t -v /var/lib/containers:/var/lib/containers -v /dev:/dev -v .:/output localhost/bootc-lldb lldb-server platform --listen "*:1234" --server
  {
    args = { "install", "to-disk", "--generic-image", "--via-loopback", "--skip-fetch-check", "~/.cache/bootc-dev/disks/test.raw" },
    cwd = "/",
    env = {
      ["RUST_LOG"] = "debug",
      ["BOOTC_DIRECT_IO"] = "on",
    },
    name = "[remote] install to-disk",
    program = program,
    request = "launch",
    console = "integratedTerminal",
    stopOnEntry = false,
    type = "lldb",
    initCommands = {
      "platform select remote-linux",
      "platform connect connect://127.0.0.1:1234", -- connect to the lldb-server running in the container
      "file target/debug/bootc",
    },
  },
}
