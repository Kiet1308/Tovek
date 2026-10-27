local record = {}
record.__index = record
record.Children = { 3, 5 }
record.People = { "Ada", "Lin" }
record.Indices = { 2, 4 }
record.Vertices = { 7, 9 }
record.Matrices = { 11, 13 }

for _, value in record.Children do print("child", value) end
for _, value in record.People do print("person", value) end
for _, value in record.Indices do print("index", value) end
for _, value in record.Vertices do print("vertex", value) end
for _, value in record.Matrices do print("matrix", value) end
print(record.__index == record)
