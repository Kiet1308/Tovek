// Execute compiler-produced bytecode, then an independently compiled driver.
// No filesystem, module loader, loadstring or network API is exposed to Luau.
#include "lua.h"
#include "lualib.h"
#include "Luau/Common.h"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iterator>
#include <string>

struct Budget
{
    size_t bytes = 0;
    std::chrono::steady_clock::time_point deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
};

static void* allocate(void* context, void* pointer, size_t oldSize, size_t newSize)
{
    Budget& budget = *static_cast<Budget*>(context);
    if (!pointer) oldSize = 0;
    if (!newSize)
    {
        budget.bytes -= oldSize;
        free(pointer);
        return nullptr;
    }
    if (newSize > 256 * 1024 * 1024 || budget.bytes - oldSize > 256 * 1024 * 1024 - newSize)
        return nullptr;
    void* result = realloc(pointer, newSize);
    if (result) budget.bytes = budget.bytes - oldSize + newSize;
    return result;
}

static std::string read(const char* path)
{
    std::ifstream stream(path, std::ios::binary);
    if (!stream) throw std::runtime_error("cannot read bytecode");
    return std::string(std::istreambuf_iterator<char>(stream), {});
}

int main(int argc, char** argv)
{
    if (argc != 3) { fprintf(stderr, "usage: benchmark-vm subject.luaubc driver.luaubc\n"); return 2; }
    // Match --fflags=false, enabling only execution of serialized CALLFB.
    for (auto* flag = Luau::FValue<bool>::list; flag; flag = flag->next)
        flag->value = strcmp(flag->name, "LuauCallFeedback") == 0;
    Budget budget;
    lua_State* state = lua_newstate(allocate, &budget);
    if (!state) return 3;
    luaL_openlibs(state);
    luaL_sandbox(state);
    luaL_sandboxthread(state);
    lua_callbacks(state)->userdata = &budget;
    lua_callbacks(state)->interrupt = [](lua_State* thread, int gc) {
        if (!gc && std::chrono::steady_clock::now() > static_cast<Budget*>(lua_callbacks(thread)->userdata)->deadline)
            luaL_error(thread, "benchmark runtime timeout");
    };
    int status = 0;
    try
    {
        std::string subject = read(argv[1]), driver = read(argv[2]);
        status = luau_load(state, "@subject", subject.data(), subject.size(), 0);
        if (!status) status = lua_pcall(state, 0, 1, 0);
        if (!status)
        {
            lua_setglobal(state, "f");
            status = luau_load(state, "@driver", driver.data(), driver.size(), 0);
            if (!status) status = lua_pcall(state, 0, 0, 0);
        }
        if (status) fprintf(stderr, "%s\n", lua_tostring(state, -1));
    }
    catch (const std::exception& error)
    {
        fprintf(stderr, "%s\n", error.what());
        status = 4;
    }
    lua_close(state);
    return status ? 1 : 0;
}
