import sys
p, e = sys.argv[1], sys.argv[2]
s = open(p).read()
a = s.index("## Evidence (head ae517c0)")
b = s.index("## Not verified")
open(p, "w").write(s[:a] + open(e).read() + s[b:])
