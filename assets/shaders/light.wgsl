#import bevy_sprite::mesh2d_vertex_output::VertexOutput

#ifdef SRGB_OUTPUT
#import bevy_render::color_operations::linear_to_srgb
#endif
#ifdef OKLAB_OUTPUT
#import bevy_render::color_operations::linear_rgb_to_oklab
#endif

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> color: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> origin_and_range: vec4<f32>;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    // Only the gradient is shaded here. Terrain ray casts define the mesh's shadow boundary.
    let radius = distance(mesh.world_position.xy, origin_and_range.xy);
    let falloff = pow(clamp(1.0 - radius / origin_and_range.z, 0.0, 1.0), 0.85);
    var output_color = vec4(color.rgb, color.a * falloff);
#ifdef VERTEX_COLORS
    // Backlit meshes store per-direction brightness in vertex alpha.
    output_color = output_color * mesh.color;
#endif
#ifdef SRGB_OUTPUT
    output_color = vec4(linear_to_srgb(output_color.rgb), output_color.a);
#endif
#ifdef OKLAB_OUTPUT
    output_color = vec4(linear_rgb_to_oklab(output_color.rgb), output_color.a);
#endif
    return output_color;
}
