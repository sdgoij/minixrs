// A record with a class and a template argument: enough for TableGen to build
// the record, resolve the field and dump it, without needing a target.
class Named<string name> {
  string Name = name;
}

def probe : Named<"probe">;
