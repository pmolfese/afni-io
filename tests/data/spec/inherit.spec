# the second surface inherits format, type and state; the third restates its state
Group = subj
StateDef = smoothwm
StateDef = pial
StateDef = inflated

NewSurface
	SurfaceFormat = ASCII
	SurfaceType = FreeSurfer
	SurfaceName = lh.smoothwm.asc
	SurfaceState = smoothwm
	Anatomical = Y
	Hemisphere = L

NewSurface
	SurfaceName = lh.pial.asc
	LocalDomainParent = lh.smoothwm.asc
	Anatomical = Y

NewSurface
	SurfaceName = lh.inflated.asc
	SurfaceState = inflated
	LocalDomainParent = lh.smoothwm.asc
	EmbedDimension = 2
