local callbacks, original = f(7)
print(callbacks[1](), callbacks[2](), original)
