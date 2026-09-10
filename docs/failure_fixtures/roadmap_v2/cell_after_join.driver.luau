for _, flag in {false, true} do
    local destroy, cleanup = f(flag)
    print(destroy())
    local destroyAgain, cleanupAgain = f(flag)
    cleanupAgain()
    print(destroyAgain())
end
