-- Run with KOReader's bundled LuaJIT, from its application directory:
-- ./luajit /path/to/repo/tests/koreader/test_plugin.lua /path/to/repo /owned/temp/dir
-- Real KOReader JSON, utilities, sorting and libarchive; only UI/network boundaries are stubbed.
local repo, temporary = assert(arg[1]), assert(arg[2])
require("setupkoenv")
assert(loadfile(repo .. "/koplugin/kindle-turbo.koplugin/_meta.lua"))
package.path = repo .. "/koplugin/kindle-turbo.koplugin/?.lua;" .. package.path
package.loaded.socketutil = { set_timeout = function() end, reset_timeout = function() end }
local util = require("util")
local lfs = require("libs/libkoreader-lfs")
local JSON = require("json")
local function read(path)
    local file = assert(io.open(path, "rb"))
    local value = file:read("*a"); file:close(); return value
end
local Source = require("kindleturboasura")
local chapters = assert(Source.props(read(repo .. "/tests/fixtures/asura/chapters.html"), "chapters")).chapters
assert(#chapters == 2 and chapters[1].number == 82 and not Source.locked(chapters[1]) and Source.locked(chapters[2]))
local pages = assert(Source.props(read(repo .. "/tests/fixtures/asura/pages.html"), "pages")).pages
assert(#pages == 2 and pages[1].url:find("001.webp", 1, true))
assert(not Source.props("<html>challenge</html>", "pages"))
local original_get = Source.get
Source.get = function() return '<astro-island props="{&quot;pages&quot;:[1,[]]}"></astro-island>' end
assert(not Source.pages("/comics/test", 1))
Source.get = function() return '<astro-island props="{&quot;pages&quot;:[1,[[0,{&quot;url&quot;:[0,&quot;https://example.test/page.webp&quot;],&quot;tiles&quot;:[1,[[0,1]]]}]]]}" />' end
assert(not Source.pages("/comics/test", 1))
Source.get = original_get
local original_json = Source.json
Source.json = function() return {series = {title = "Null fixture", public_url = "/comics/null-fixture"}} end
Source.get = function() return '<astro-island props="{&quot;chapters&quot;:[1,[[0,{&quot;number&quot;:[0,1],&quot;title&quot;:[0,null]}]]]}" />' end
local null_chapters = assert(Source.chapters("null-fixture"))
assert(#null_chapters == 1 and null_chapters[1].title == "")
Source.get, Source.json = original_get, original_json
print("PASS real Astro fixtures, locks, missing/empty/scrambled pages")

local scheduled, messages, standby = {}, {}, 0
local UI = {
    show = function(_, widget) if widget.text then messages[#messages + 1] = widget.text end end,
    close = function() end,
    scheduleIn = function(_, _, callback) scheduled[#scheduled + 1] = callback end,
    preventStandby = function() standby = standby + 1 end,
    allowStandby = function() standby = standby - 1; assert(standby >= 0) end,
}
package.loaded["ui/uimanager"] = UI
local widget = { new = function(_, value) return value end }
for _, name in ipairs({"menu", "inputdialog", "infomessage", "buttondialog", "confirmbox"}) do
    package.loaded["ui/widget/" .. name] = widget
end
package.loaded["ui/widget/container/widgetcontainer"] = { extend = function(_, value) return value end }
package.loaded["ui/network/manager"] = { runWhenConnected = function(_, callback) callback() end }
package.loaded["apps/reader/readerui"] = { showReader = function() end }
local Turbo = dofile(repo .. "/koplugin/kindle-turbo.koplugin/main.lua")
local turbo = setmetatable({root = temporary .. "/downloads", ui = {}}, {__index = Turbo})
local details = {title = "Fixture", slug = "fixture", public_url = "/comics/fixture"}
local chapter = {number = 1}
local path = turbo:chapterPath(details, chapter)
local chapter_url = turbo:chapterURL(details, chapter)
local function drain()
    local count = 0
    while #scheduled > 0 do
        count = count + 1; assert(count < 100, "download loop did not terminate")
        table.remove(scheduled, 1)()
    end
end
-- Valid PNG generated with standard zlib, no copyrighted image fixture.
local image = read(repo .. "/tests/fixtures/asura/page.png")
Source.pages = function() return {"https://example.test/1.png", "https://example.test/2.png"} end
Source.get = function() return image end
turbo:startDownloads(details, {chapter})
drain()
assert(not turbo.job and lfs.attributes(path, "mode") == "file")
assert(not lfs.attributes(path .. ".part"))
assert(turbo:archiveValid(path, chapter_url))
assert(not turbo:archiveValid(path, "different chapter"))
local archive = require("ffi/archiver").Reader:new()
assert(archive:open(path))
for entry in archive:iterate() do
    if entry.path:match("%.png$") then assert(archive:extractToMemory(entry.index) == image) end
end
archive:close()
local calls = 0
Source.get = function() calls = calls + 1; return image end
turbo:startDownloads(details, {chapter})
drain()
assert(calls == 0 and not turbo.job)
print("PASS native libarchive CBZ, image bytes, identity and completed-download reuse")

local function next_path(number)
    return turbo:chapterPath(details, {number = number})
end
-- Cancellation after Writer:open and before the scheduled first page request.
turbo:startDownloads(details, {{number = 2}})
table.remove(scheduled, 1)()
assert(turbo.job.writer and lfs.attributes(next_path(2) .. ".part"))
turbo:finishDownload("cancelled")
drain()
assert(not lfs.attributes(next_path(2)) and not lfs.attributes(next_path(2) .. ".part"))
assert(turbo:archiveValid(path, chapter_url))
local foreign_partial = next_path(3) .. ".part"
local file = assert(io.open(foreign_partial, "wb")); file:write("another download"); file:close()
turbo:startDownloads(details, {{number = 3}})
drain()
assert(not turbo.job and read(foreign_partial) == "another download")
Source.get = function() return "<html>challenge</html>" end
turbo:startDownloads(details, {{number = 4}})
drain()
assert(not turbo.job and not lfs.attributes(next_path(4)) and not lfs.attributes(next_path(4) .. ".part"))
assert(standby == 0 and require("pluginshare").pause_auto_suspend == nil)
print("PASS cancellation, partial ownership, HTTP-image rejection and cleanup")
