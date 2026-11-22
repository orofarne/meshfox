<!-- meshfox:canvas -->
# Navier–Stokes Equations
<!-- meshfox:node id="root" -->

A worked example of **math in meshfox**: `$…$` and `$$…$$` (KaTeX in the web
UI, RaTeX-typeset images in the TUI — see MARKDOWN.md, "Math"), laid out as a
small canvas. The text is adapted from NASA Glenn Research Center's
*Beginner's Guide to Aeronautics* page [Navier-Stokes Equations](https://www.grc.nasa.gov/www/BGH/nseqs.html)
(see [Attribution](#attribution) below); the equations are re-typeset in LaTeX.

The equations describe how the velocity, pressure, temperature and density of
a moving fluid are related. They were derived independently by G. G. Stokes in
England and M. Navier in France in the early 1800s, and extend the Euler
equations with the effects of viscosity.

## The equations
<!-- meshfox:node id="equations" -->

Four independent variables: the spatial coordinates $x, y, z$ and the time
$t$. Six dependent variables: the pressure $p$, density $\rho$ and temperature
$T$ (carried by the total energy $E_t$), and the velocity components $u, v, w$.
All of them are functions of all four independent variables, so these are
*partial* differential equations.

**Continuity**

$$
\frac{\partial \rho}{\partial t}
+ \frac{\partial (\rho u)}{\partial x}
+ \frac{\partial (\rho v)}{\partial y}
+ \frac{\partial (\rho w)}{\partial z} = 0
$$

**X-momentum** (the y- and z-momentum equations follow the same pattern)

$$
\frac{\partial (\rho u)}{\partial t}
+ \frac{\partial (\rho u^2)}{\partial x}
+ \frac{\partial (\rho u v)}{\partial y}
+ \frac{\partial (\rho u w)}{\partial z}
= -\frac{\partial p}{\partial x}
+ \frac{1}{\mathrm{Re}}\left(
  \frac{\partial \tau_{xx}}{\partial x}
+ \frac{\partial \tau_{xy}}{\partial y}
+ \frac{\partial \tau_{xz}}{\partial z}\right)
$$

**Energy**

$$
\begin{aligned}
&\frac{\partial E_t}{\partial t}
+ \frac{\partial (u E_t)}{\partial x}
+ \frac{\partial (v E_t)}{\partial y}
+ \frac{\partial (w E_t)}{\partial z} \\
&\quad = -\frac{\partial (p u)}{\partial x}
  -\frac{\partial (p v)}{\partial y}
  -\frac{\partial (p w)}{\partial z}
  - \frac{1}{\mathrm{Re}\,\mathrm{Pr}}\left(
    \frac{\partial q_x}{\partial x}
  + \frac{\partial q_y}{\partial y}
  + \frac{\partial q_z}{\partial z}\right) \\
&\qquad + \frac{1}{\mathrm{Re}}\left[
    \frac{\partial (u\tau_{xx} + v\tau_{xy} + w\tau_{xz})}{\partial x}
  + \frac{\partial (u\tau_{xy} + v\tau_{yy} + w\tau_{yz})}{\partial y}
  + \frac{\partial (u\tau_{xz} + v\tau_{yz} + w\tau_{zz})}{\partial z}\right]
\end{aligned}
$$

> [!NOTE]
> The source page writes the energy equation with $\rho u$ in the pressure
> terms and $w\tau_{xz}$ in the y-derivative. Both are typos in the original
> (the pressure work is $p\,\mathbf{u}$ and the stress work in the y-term is
> $w\tau_{yz}$); this example uses the corrected forms.

The system is *coupled*: every dependent variable appears in each equation, so
all five equations are solved together. Five equations for six unknowns also
need an equation of state relating $p$, $T$ and $\rho$, and a closure for the
stress tensor $\tau_{ij}$ — in CFD usually a turbulence model.
<!-- meshfox:edge from="terms" label="explains the terms" -->

## Convection and diffusion
<!-- meshfox:node id="terms" -->

The left-hand side of each momentum equation holds the **convection** terms:
a property is transported by the ordered motion of the flow. The right-hand
terms multiplied by $1/\mathrm{Re}$ are the **diffusion** terms: transport by
the random motion of the molecules of the gas. Diffusion ties to the stress
tensor and the viscosity of the gas; turbulence and boundary layers are its
result.

The Euler equations keep only the convection terms, so they cannot model
boundary layers.

| Symbol | Meaning |
|---|---|
| $\mathrm{Re}$ | Reynolds number: inertia of the flow relative to its viscous forces |
| $\mathrm{Pr}$ | Prandtl number: viscous relative to thermal diffusion |
| $\tau_{ij}$ | components of the stress tensor (nine, symmetric) |
| $q_x, q_y, q_z$ | heat-flux components |

## A Reynolds number
<!-- meshfox:node id="reynolds" -->

The Reynolds number compares the inertia of a flow with its viscous forces:

$$
\mathrm{Re} = \frac{\rho U L}{\mu}
$$

Take air at sea level ($\rho \approx 1.225\,\mathrm{kg/m^3}$,
$\mu \approx 1.81\cdot 10^{-5}\,\mathrm{Pa\cdot s}$) flowing at
$U = 70\,\mathrm{m/s}$ over a wing chord $L = 1.5\,\mathrm{m}$. The block
below is a computed view: it runs in the sandbox, prints Markdown — formula
included — and shows the result in place of itself, never writing it into
this file.

```starlark render name="reynolds" output="markdown"
rho = 1.225   # kg/m^3
mu = 1.81e-5  # Pa*s
U = 70.0      # m/s
L = 1.5       # m

re = rho * U * L / mu
# Scientific notation by hand: re = mantissa * 10^exponent, 1 <= mantissa < 10.
exponent = 0
mantissa = re
for _ in range(300):
    if mantissa < 10:
        break
    mantissa = mantissa / 10
    exponent += 1
mantissa = int(mantissa * 100 + 0.5) / 100.0

print("$$\n\\mathrm{Re} = \\frac{%s \\cdot %s \\cdot %s}{%s} \\approx %s \\times 10^{%d}\n$$" % (
    rho, U, L, "1.81 \\cdot 10^{-5}", mantissa, exponent,
))
```

## Attribution
<!-- meshfox:node id="attribution" -->

Text adapted from *Navier-Stokes Equations*, NASA Glenn Research Center,
*Beginner's Guide to Aeronautics* (editor and NASA official: Tom Benson, last
updated 7 May 2021): <https://www.grc.nasa.gov/www/BGH/nseqs.html>.

NASA content is generally not subject to copyright in the United States
(<https://www.nasa.gov/nasa-brand-center/images-and-media/>); NASA is
acknowledged as the source. This is an adaptation, not a NASA publication: it
was restructured into a canvas, the equations were re-typeset in LaTeX with
the two corrections noted above, and a worked example was added. NASA does not
endorse it, and no NASA logos or page images are used.
