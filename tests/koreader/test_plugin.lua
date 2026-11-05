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
local http = require("socket.http")
local original_request, sent_request = http.request, nil
http.request = function(request) sent_request = request; request.sink("body"); return 1, 200 end
local transport = require("kindleturbohttp")
assert(transport.get("https://api.asurascans.com/api/series", "https://asurascans.com", 0) == "body")
assert(sent_request.headers.Accept:find("text/html", 1, true))
assert(transport.get("https://official.lowee.us/page.png", "https://weebcentral.com", 0, true) == "body")
assert(sent_request.headers.Accept:find("image/", 1, true))
assert(sent_request.headers.Referer == "https://weebcentral.com/" and sent_request.headers.Origin == nil)
http.request = original_request
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
Source.json = function() return {data = {series = {title = "Wrapped fixture", public_url = "/comics/null-fixture"}}} end
local wrapped_chapters, wrapped_error, wrapped_details = Source.chapters("null-fixture")
assert(wrapped_chapters, wrapped_error)
assert(wrapped_details.title == "Wrapped fixture" and #wrapped_chapters == 1)
Source.get, Source.json = original_get, original_json
print("PASS real Astro fixtures, locks, missing/empty/scrambled pages")

local Weeb = require("kindleturboweebcentral")
local weeb_items, weeb_err, weeb_more = Weeb.parseSearch(read(repo .. "/tests/fixtures/weebcentral/search.html"))
assert(weeb_items, weeb_err)
assert(#weeb_items == 2 and weeb_items[2].title == "Series & Examples" and weeb_more)
local weeb_chapters = assert(Weeb.parseChapters(read(repo .. "/tests/fixtures/weebcentral/chapters.html")))
assert(#weeb_chapters == 4 and weeb_chapters[1].id ~= weeb_chapters[4].id)
assert(weeb_chapters[2].title == "Special" and weeb_chapters[3].title == "Volume 1")
local weeb_pages = assert(Weeb.parsePages(read(repo .. "/tests/fixtures/weebcentral/pages.html")))
assert(#weeb_pages == 2 and weeb_pages[1] == "https://official.lowee.us/fixture/001.png?x=1&y=2")
assert(not Weeb.parsePages("<html>challenge</html>"))
assert(not Weeb.parseChapters("<html>challenge</html>"))
assert(not Weeb.parseSearch("<html>challenge</html>"))
local large_catalog = {}
for index = 1, 196 do
    large_catalog[#large_catalog + 1] = string.format('<div x-data="{}"><a href="/chapters/%026d"><span class="flex"><span>Chapter %d</span><span><svg><path></path></svg></span></span><time>date</time></a><input><input></div>', index, index)
end
assert(#assert(Weeb.parseChapters(table.concat(large_catalog))) == 196, "native parser must not truncate the full catalog at 1000 iterations")
local weeb_get = Weeb.get
Weeb.get = function(url)
    if url:find("full-chapter-list", 1, true) then return read(repo .. "/tests/fixtures/weebcentral/chapters.html") end
    if url:find("/images?", 1, true) then return read(repo .. "/tests/fixtures/weebcentral/pages.html") end
    return read(repo .. "/tests/fixtures/weebcentral/details.html")
end
local weeb_list, details_error, weeb_details = Weeb.chapters(weeb_items[1].slug)
assert(weeb_list, details_error)
assert(weeb_details.title == "Series & Examples" and weeb_details.slug == weeb_items[1].slug)
assert(#assert(Weeb.pages(weeb_details.public_url, nil, weeb_list[1])) == 2)
Weeb.get = weeb_get
print("PASS WeebCentral HTML, opaque IDs, decimals, volumes/extras, entities and image order")

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

weeb_details.source = Weeb
Weeb.pages = function() return {"https://example.test/weeb-1.png", "https://example.test/weeb-2.png"} end
Weeb.get = function() return image end
turbo:startDownloads(weeb_details, weeb_chapters)
drain()
assert(not turbo.job)
local weeb_paths = {}
for _, chapter in ipairs(weeb_chapters) do
    local chapter_path = turbo:chapterPath(weeb_details, chapter)
    assert(not weeb_paths[chapter_path] and chapter_path ~= path)
    weeb_paths[chapter_path] = true
    assert(turbo:archiveValid(chapter_path, chapter.url))
    local reader = require("ffi/archiver").Reader:new()
    assert(reader:open(chapter_path))
    for entry in reader:iterate() do
        if entry.path == "kindle-turbo.json" then
            assert(JSON.decode(reader:extractToMemory(entry.index)).source == "weebcentral")
        end
    end
    reader:close()
end
assert(turbo:archiveValid(path, chapter_url))
assert(standby == 0 and require("pluginshare").pause_auto_suspend == nil)
print("PASS WeebCentral CBZ jobs use their own source, unique paths and manifests; Asura downloads preserved")

local original_show, original_asura_search, original_weeb_search, original_weeb_chapters = turbo.showMenu, Source.search, Weeb.search, Weeb.chapters
local menu_items, selected_provider
turbo.showMenu = function(_, _, items) menu_items = items; return {} end
Source.search = function() return {{title = "Asura", slug = "asura"}}, nil, false end
Weeb.search = function() return {{title = "Weeb", slug = "weeb"}}, nil, false end
Weeb.chapters = function() selected_provider = Weeb; return {}, nil, {title = "Weeb", slug = "weeb"} end
turbo:catalog("weeb", 1, Weeb)
local weeb_callback = menu_items[1].callback
turbo:catalog("asura", 1, Source)
weeb_callback()
assert(selected_provider == Weeb)
turbo.showMenu, Source.search, Weeb.search, Weeb.chapters = original_show, original_asura_search, original_weeb_search, original_weeb_chapters
print("PASS catalog callbacks retain the selected source across other catalog interactions")
