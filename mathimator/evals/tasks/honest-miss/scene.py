# mathimator: scene api=1
# %% [c1] A curve already on the canvas
from mathimator import sym, plot
x = sym.symbols("x")
plot.explicit(sym.define("sin(x)"), x=(-10, 10), id="wave")
