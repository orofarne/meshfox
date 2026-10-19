-- Filetype detection for meshfox canvases: `markdown.meshfox`, i.e. still
-- Markdown (its ftplugins/plugins/LSP keep working, and vim.treesitter maps
-- the compound filetype to the `markdown` parser) plus a `meshfox` layer for
-- settings that should only apply to canvases.
--
-- A canvas is either named `*.canvas.md`, or any `*.md` whose first line is
-- the `<!-- meshfox:canvas -->` marker (README.md is one).
local function is_canvas_marker(_, bufnr)
  local first = vim.api.nvim_buf_get_lines(bufnr, 0, 1, false)[1]
  if first and first:match('^%s*<!%-%-%s*meshfox:canvas') then
    return 'markdown.meshfox'
  end
end

vim.filetype.add({
  pattern = {
    ['.*%.canvas%.md'] = { 'markdown.meshfox', { priority = 10 } },
    ['.*%.md'] = { is_canvas_marker, { priority = 10 } },
  },
})

-- Language server: `meshfox lsp` (diagnostics, a node outline, and the
-- "turn this heading into a node" code action -- `gra` on a heading).
-- Needs Neovim 0.11+ (`vim.lsp.config`) and a `meshfox` on $PATH new enough
-- to have the `lsp` subcommand; that is probed once, on the first canvas you
-- open, so an older binary just means no server rather than an error on
-- every file. Point `vim.g.meshfox_lsp_cmd` at another command (a table) to
-- use a different binary (not probed), or set `vim.g.meshfox_lsp_disable =
-- true` to turn the server off.
local function enable_lsp(cmd)
  vim.lsp.config('meshfox', {
    cmd = cmd,
    filetypes = { 'markdown.meshfox' },
    root_markers = { '.meshfox', '.git' },
  })
  vim.lsp.enable('meshfox')
  -- `enable` attaches already-open buffers itself only once startup has
  -- finished; the probe below can return earlier than that, with the
  -- first canvas (`nvim file.canvas.md`) already open.
  vim.cmd.doautoall('nvim.lsp.enable FileType')
end

if vim.lsp and vim.lsp.config and vim.lsp.enable and not vim.g.meshfox_lsp_disable then
  vim.api.nvim_create_autocmd('FileType', {
    pattern = 'markdown.meshfox',
    once = true,
    callback = function()
      if vim.g.meshfox_lsp_cmd then
        return enable_lsp(vim.g.meshfox_lsp_cmd)
      end
      if vim.fn.executable('meshfox') ~= 1 then
        return
      end
      -- `meshfox lsp --help` exits 0 only where the subcommand exists.
      vim.system({ 'meshfox', 'lsp', '--help' }, {}, function(res)
        if res.code == 0 then
          vim.schedule(function() enable_lsp({ 'meshfox', 'lsp' }) end)
        end
      end)
    end,
  })
end

-- `<leader>mn`: turn the heading under the cursor into a meshfox node -- the
-- language server's "Turn ... into a meshfox node" code action applied in one
-- keystroke (the counterpart of `Ctrl-n` in the TUI's source editor), instead
-- of picking it from the `gra` menu. Buffer-local, only in canvases; set
-- `vim.g.meshfox_no_mappings = true` to skip it.
if not vim.g.meshfox_no_mappings then
  vim.api.nvim_create_autocmd('FileType', {
    pattern = 'markdown.meshfox',
    callback = function(ev)
      vim.keymap.set('n', '<leader>mn', function()
        vim.lsp.buf.code_action({
          apply = true,
          filter = function(action)
            -- The single-heading action, not the "all plain headings" one.
            return action.title:match('^Turn ".*" into a meshfox node$') ~= nil
          end,
        })
      end, { buffer = ev.buf, desc = '[M]eshfox: heading to [N]ode' })
    end,
  })
end
